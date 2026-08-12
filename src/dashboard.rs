use askama::Template;
use base64::Engine;
use chrono::Utc;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use tokio::sync::broadcast;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Allowed,
    Blocked,
    CacheHit,
    Failed,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Self::Allowed => "Allowed",
            Self::Blocked => "Blocked",
            Self::CacheHit => "Cache hit",
            Self::Failed => "Failed",
        }
    }

    fn class(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Blocked => "blocked",
            Self::CacheHit => "cached",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug)]
pub struct EventInput {
    pub outcome: Outcome,
    pub model: String,
    pub route: String,
    pub provider: String,
    pub reason: String,
    pub cost_usd: f64,
    pub elapsed_ms: u64,
}

#[derive(Clone, Debug)]
pub struct DashboardEvent {
    pub id: u64,
    pub timestamp: String,
    pub outcome: Outcome,
    pub model: String,
    pub route: String,
    pub provider: String,
    pub reason: String,
    pub cost_usd: f64,
    pub elapsed_ms: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DashboardSummary {
    pub total: u64,
    pub allowed: u64,
    pub blocked: u64,
    pub cache_hits: u64,
    pub failed: u64,
}

pub struct Dashboard {
    capacity: usize,
    next_id: AtomicU64,
    events: RwLock<VecDeque<DashboardEvent>>,
    totals: RwLock<DashboardSummary>,
    sender: broadcast::Sender<DashboardEvent>,
}

impl Dashboard {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self {
            capacity: capacity.max(1),
            next_id: AtomicU64::new(1),
            events: RwLock::new(VecDeque::with_capacity(capacity.max(1))),
            totals: RwLock::new(DashboardSummary::default()),
            sender,
        }
    }

    pub fn record(&self, input: EventInput) -> DashboardEvent {
        let event = DashboardEvent {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            timestamp: Utc::now().format("%H:%M:%S").to_string(),
            outcome: input.outcome,
            model: input.model,
            route: input.route,
            provider: input.provider,
            reason: input.reason,
            cost_usd: input.cost_usd,
            elapsed_ms: input.elapsed_ms,
        };

        {
            let mut totals = self.totals.write().unwrap();
            totals.total += 1;
            match event.outcome {
                Outcome::Allowed => totals.allowed += 1,
                Outcome::Blocked => totals.blocked += 1,
                Outcome::CacheHit => totals.cache_hits += 1,
                Outcome::Failed => totals.failed += 1,
            }
        }

        {
            let mut events = self.events.write().unwrap();
            events.push_front(event.clone());
            events.truncate(self.capacity);
        }
        let _ = self.sender.send(event.clone());
        event
    }

    pub fn snapshot(&self) -> Vec<DashboardEvent> {
        self.events.read().unwrap().iter().cloned().collect()
    }

    pub fn summary(&self) -> DashboardSummary {
        *self.totals.read().unwrap()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DashboardEvent> {
        self.sender.subscribe()
    }
}

pub fn api_key_from_basic(header: &str) -> Option<String> {
    let encoded = header.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let credentials = String::from_utf8(decoded).ok()?;
    let (_, password) = credentials.split_once(':')?;
    (!password.is_empty()).then(|| password.to_string())
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Snapshot {
    pub summary: DashboardSummary,
    pub spend_usd: f64,
    pub limit_usd: f64,
    pub reserved_usd: f64,
    pub healthy_nodes: usize,
    pub total_nodes: usize,
}

#[derive(Template)]
#[template(path = "dashboard.html")]
struct DashboardTemplate {
    events_html: String,
}

#[derive(Template)]
#[template(path = "dashboard_summary.html")]
struct SummaryTemplate {
    spend: String,
    limit: String,
    reserved: String,
    budget_percent: String,
    total: u64,
    allowed: u64,
    blocked: u64,
    cache_hits: u64,
    failed: u64,
    healthy_nodes: usize,
    total_nodes: usize,
}

pub fn render_page(events: &[DashboardEvent]) -> Result<String, askama::Error> {
    let events_html = if events.is_empty() {
        "<div class=\"empty\">Waiting for traffic…<br>Send an OpenAI-compatible request through Stoke.</div>".to_string()
    } else {
        events
            .iter()
            .map(render_event)
            .collect::<Result<Vec<_>, _>>()?
            .join("")
    };
    DashboardTemplate { events_html }.render()
}

pub fn render_summary(snapshot: &Snapshot) -> Result<String, askama::Error> {
    let budget_percent = if snapshot.limit_usd > 0.0 {
        ((snapshot.spend_usd + snapshot.reserved_usd) / snapshot.limit_usd * 100.0)
            .clamp(0.0, 100.0)
    } else {
        0.0
    };
    SummaryTemplate {
        spend: format!("${:.4}", snapshot.spend_usd),
        limit: if snapshot.limit_usd > 0.0 {
            format!("${:.4}", snapshot.limit_usd)
        } else {
            "Unlimited".to_string()
        },
        reserved: format!("${:.4}", snapshot.reserved_usd),
        budget_percent: format!("{budget_percent:.2}"),
        total: snapshot.summary.total,
        allowed: snapshot.summary.allowed,
        blocked: snapshot.summary.blocked,
        cache_hits: snapshot.summary.cache_hits,
        failed: snapshot.summary.failed,
        healthy_nodes: snapshot.healthy_nodes,
        total_nodes: snapshot.total_nodes,
    }
    .render()
}

#[derive(Template)]
#[template(path = "dashboard_event.html")]
struct EventTemplate<'a> {
    id: u64,
    timestamp: &'a str,
    outcome_label: &'static str,
    outcome_class: &'static str,
    model: &'a str,
    route: &'a str,
    provider: &'a str,
    reason: &'a str,
    elapsed: String,
    cost: String,
}

pub fn node_counts(snapshot: &serde_json::Value) -> (usize, usize) {
    let Some(nodes) = snapshot.get("nodes") else {
        return (0, 0);
    };
    if let Some(nodes) = nodes.as_array() {
        let healthy = nodes
            .iter()
            .filter(|node| node.get("healthy").and_then(serde_json::Value::as_bool) == Some(true))
            .count();
        return (healthy, nodes.len());
    }
    let Some(nodes) = nodes.as_object() else {
        return (0, 0);
    };
    let healthy = nodes
        .values()
        .filter(|node| node.get("healthy").and_then(serde_json::Value::as_bool) == Some(true))
        .count();
    (healthy, nodes.len())
}

pub fn render_event(event: &DashboardEvent) -> askama::Result<String> {
    EventTemplate {
        id: event.id,
        timestamp: &event.timestamp,
        outcome_label: event.outcome.label(),
        outcome_class: event.outcome.class(),
        model: &event.model,
        route: &event.route,
        provider: &event.provider,
        reason: &event.reason,
        elapsed: (event.elapsed_ms > 0)
            .then(|| format!("{} ms", event.elapsed_ms))
            .unwrap_or_default(),
        cost: (event.cost_usd > 0.0)
            .then(|| format!("${:.6}", event.cost_usd))
            .unwrap_or_default(),
    }
    .render()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(outcome: Outcome, reason: &str) -> EventInput {
        EventInput {
            outcome,
            model: "configured-model".into(),
            route: "single".into(),
            provider: "local-node".into(),
            reason: reason.into(),
            cost_usd: 0.0025,
            elapsed_ms: 42,
        }
    }

    #[test]
    fn event_log_keeps_only_the_newest_events() {
        let dashboard = Dashboard::new(2);
        dashboard.record(event(Outcome::Allowed, "first"));
        dashboard.record(event(Outcome::Blocked, "second"));
        dashboard.record(event(Outcome::Failed, "third"));

        let events = dashboard.snapshot();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].reason, "third");
        assert_eq!(events[1].reason, "second");
    }

    #[test]
    fn summary_counts_enforcement_outcomes() {
        let dashboard = Dashboard::new(10);
        dashboard.record(event(Outcome::Allowed, "served"));
        dashboard.record(event(Outcome::Blocked, "loop detected"));
        dashboard.record(event(Outcome::CacheHit, "cache hit"));
        dashboard.record(event(Outcome::Failed, "upstream failed"));

        let summary = dashboard.summary();
        assert_eq!(summary.total, 4);
        assert_eq!(summary.allowed, 1);
        assert_eq!(summary.blocked, 1);
        assert_eq!(summary.cache_hits, 1);
        assert_eq!(summary.failed, 1);
    }

    #[test]
    fn basic_auth_uses_the_password_as_the_api_key() {
        assert_eq!(
            api_key_from_basic("Basic c3Rva2U6c2VjcmV0LWtleQ=="),
            Some("secret-key".to_string())
        );
    }

    #[test]
    fn basic_auth_rejects_other_schemes_and_missing_passwords() {
        assert_eq!(api_key_from_basic("Bearer secret-key"), None);
        assert_eq!(api_key_from_basic("Basic c3Rva2U="), None);
    }

    #[test]
    fn dashboard_page_connects_htmx_to_the_live_event_stream() {
        let dashboard = Dashboard::new(10);
        dashboard.record(event(Outcome::Allowed, "served locally"));
        let html = render_page(&dashboard.snapshot()).unwrap();

        assert!(html.contains("/ui/htmx.min.js"));
        assert!(html.contains("/ui/sse.js"));
        assert!(html.contains("sse-connect=\"/ui/events\""));
        assert!(html.contains("/ui/demo"));
        assert!(html.contains("Run enforcement demo"));
        assert!(html.contains("served locally"));
    }

    #[test]
    fn summary_renders_budget_and_decision_totals() {
        let snapshot = Snapshot {
            summary: DashboardSummary {
                total: 7,
                allowed: 4,
                blocked: 2,
                cache_hits: 1,
                failed: 0,
            },
            spend_usd: 1.25,
            limit_usd: 5.0,
            reserved_usd: 0.5,
            healthy_nodes: 2,
            total_nodes: 3,
        };
        let html = render_summary(&snapshot).unwrap();

        assert!(html.contains("$1.2500"));
        assert!(html.contains("$5.0000"));
        assert!(html.contains("2 / 3"));
        assert!(html.contains(">2<"));
    }

    #[test]
    fn node_counts_read_the_registry_array_shape() {
        let registry = serde_json::json!({
            "nodes": [
                { "name": "warm", "healthy": true },
                { "name": "cold", "healthy": false }
            ]
        });

        assert_eq!(node_counts(&registry), (1, 2));
    }

    #[test]
    fn node_counts_read_the_registry_object_shape() {
        let registry = serde_json::json!({
            "nodes": {
                "warm": { "healthy": true },
                "cold": { "healthy": false }
            }
        });

        assert_eq!(node_counts(&registry), (1, 2));
    }

    #[test]
    fn rendered_event_escapes_provider_text() {
        let dashboard = Dashboard::new(1);
        let recorded = dashboard.record(event(Outcome::Blocked, "<script>alert(1)</script>"));

        let html = render_event(&recorded).unwrap();
        assert!(!html.contains("<script>"));
        assert!(html.contains("&#60;script&#62;"));
    }
}
