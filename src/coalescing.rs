use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;

const DEFAULT_MAX_INFLIGHT: usize = 1024;
const DEFAULT_WAIT: Duration = Duration::from_secs(5);

struct Flight {
    completed: watch::Sender<bool>,
}

struct Inner {
    flights: Mutex<HashMap<String, Arc<Flight>>>,
}

/// A bounded, route-local registry for requests that may share an exact cache result.
/// It never stores a response: the existing ResponseCache remains the sole result store.
pub struct Coalescer {
    inner: Arc<Inner>,
    max_inflight: usize,
    wait_duration: Duration,
}

pub enum Claim {
    Leader(LeaderGuard),
    Follower(Follower),
    /// The registry is full; the caller should use its normal dispatch path.
    Bypass,
}

pub struct Follower {
    completed: watch::Receiver<bool>,
    wait_duration: Duration,
}

impl Follower {
    /// True means the flight ended; the caller must still re-read exact cache.
    /// False means the bounded wait elapsed and normal dispatch may proceed.
    pub async fn wait(mut self) -> bool {
        if *self.completed.borrow() {
            return true;
        }
        match tokio::time::timeout(self.wait_duration, self.completed.changed()).await {
            Ok(Ok(())) | Ok(Err(_)) => true,
            Err(_) => false,
        }
    }
}

/// Drops on every leader exit path. Removal happens before wake-up so a waiter
/// that cannot share may become the next leader without a stale registration.
pub struct LeaderGuard {
    inner: Arc<Inner>,
    key: String,
    flight: Arc<Flight>,
}

impl Coalescer {
    pub fn new(max_inflight: usize, wait_duration: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                flights: Mutex::new(HashMap::new()),
            }),
            max_inflight,
            wait_duration,
        }
    }

    pub fn default() -> Self {
        Self::new(DEFAULT_MAX_INFLIGHT, DEFAULT_WAIT)
    }

    pub fn claim(&self, key: &str) -> Claim {
        if self.max_inflight == 0 {
            return Claim::Bypass;
        }
        let mut flights = self.inner.flights.lock().unwrap();
        if let Some(flight) = flights.get(key) {
            return Claim::Follower(Follower {
                completed: flight.completed.subscribe(),
                wait_duration: self.wait_duration,
            });
        }
        if flights.len() >= self.max_inflight {
            return Claim::Bypass;
        }
        let (completed, _receiver) = watch::channel(false);
        let flight = Arc::new(Flight { completed });
        flights.insert(key.to_string(), Arc::clone(&flight));
        Claim::Leader(LeaderGuard {
            inner: Arc::clone(&self.inner),
            key: key.to_string(),
            flight,
        })
    }
}

impl Drop for LeaderGuard {
    fn drop(&mut self) {
        let mut flights = self.inner.flights.lock().unwrap();
        if flights
            .get(&self.key)
            .is_some_and(|current| Arc::ptr_eq(current, &self.flight))
        {
            flights.remove(&self.key);
        }
        drop(flights);
        let _ = self.flight.completed.send(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::time::{sleep, timeout};

    #[tokio::test]
    async fn cold_overlap_has_one_leader_and_joined_waiter() {
        let coalescer = Coalescer::new(8, Duration::from_secs(1));
        let leader = match coalescer.claim("same-full-identity") {
            Claim::Leader(guard) => guard,
            _ => panic!("first request must lead"),
        };
        let follower = match coalescer.claim("same-full-identity") {
            Claim::Follower(waiter) => waiter,
            _ => panic!("second request must join"),
        };

        drop(leader);
        assert!(
            follower.wait().await,
            "leader completion must wake follower"
        );
        assert!(matches!(
            coalescer.claim("same-full-identity"),
            Claim::Leader(_)
        ));
    }

    #[tokio::test]
    async fn timeout_is_bounded_and_follower_cancellation_is_independent() {
        let coalescer = Coalescer::new(8, Duration::from_millis(20));
        let _leader = match coalescer.claim("identity") {
            Claim::Leader(guard) => guard,
            _ => panic!("first request must lead"),
        };
        let follower = match coalescer.claim("identity") {
            Claim::Follower(waiter) => waiter,
            _ => panic!("second request must join"),
        };
        let started = tokio::time::Instant::now();
        assert!(!follower.wait().await);
        assert!(started.elapsed() >= Duration::from_millis(15));

        let canceled = match coalescer.claim("identity") {
            Claim::Follower(waiter) => tokio::spawn(async move { waiter.wait().await }),
            _ => panic!("third request must still join"),
        };
        canceled.abort();
        sleep(Duration::from_millis(1)).await;
        assert!(matches!(coalescer.claim("identity"), Claim::Follower(_)));
    }

    #[tokio::test]
    async fn leader_drop_cleans_failed_or_noncacheable_flight() {
        let coalescer = Coalescer::new(1, Duration::from_secs(1));
        let leader = match coalescer.claim("failed") {
            Claim::Leader(guard) => guard,
            _ => panic!("first request must lead"),
        };
        let follower = match coalescer.claim("failed") {
            Claim::Follower(waiter) => waiter,
            _ => panic!("second request must join"),
        };
        drop(leader);
        assert!(follower.wait().await, "failure must still wake the waiter");
        assert!(matches!(coalescer.claim("failed"), Claim::Leader(_)));
    }

    #[tokio::test]
    async fn distinct_identity_and_overflow_bypass_without_registration() {
        let coalescer = Coalescer::new(1, Duration::from_secs(1));
        let _first = match coalescer.claim("identity-a") {
            Claim::Leader(guard) => guard,
            _ => panic!("first identity must lead"),
        };
        assert!(matches!(coalescer.claim("identity-b"), Claim::Bypass));
        assert!(matches!(coalescer.claim("identity-a"), Claim::Follower(_)));
    }

    #[tokio::test]
    async fn waiter_wakes_without_hanging_when_leader_is_canceled() {
        let coalescer = Arc::new(Coalescer::new(8, Duration::from_secs(1)));
        let leader = match coalescer.claim("canceled-leader") {
            Claim::Leader(guard) => guard,
            _ => panic!("first request must lead"),
        };
        let waiter = match coalescer.claim("canceled-leader") {
            Claim::Follower(waiter) => waiter,
            _ => panic!("second request must join"),
        };
        let leader_task = tokio::spawn(async move {
            let _leader = leader;
            sleep(Duration::from_secs(60)).await;
        });
        let waiter_task = tokio::spawn(async move { waiter.wait().await });
        leader_task.abort();
        assert!(timeout(Duration::from_secs(1), waiter_task)
            .await
            .unwrap()
            .unwrap());
        assert!(matches!(coalescer.claim("canceled-leader"), Claim::Leader(_)));
    }
}
