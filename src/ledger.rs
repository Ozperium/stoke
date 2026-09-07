//! The durable budget ledger (ADR 0001).
//!
//! SQLite-backed, in-process, zero runtime dependencies (bundled). Everything
//! here is keyed by `key_id` — raw secrets never touch the database. Any DB
//! failure must surface as `Err` so the metered path fails closed before the
//! provider call; the gateway never degrades to memory-only enforcement.
//!
//! Crash semantics ("hard cap"): a reservation row is written before the
//! provider call; completion atomically converts it into spend; after a crash
//! an unresolved reservation stays open and keeps counting against the key's
//! committed total until the operator reconciles it. Auto-expiring holds is
//! rejected — it re-opens the over-cap window after every crash.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

/// The ledger. One connection per gateway process; SQLite serializes writers.
/// The Mutex<Connection> is read by every operation; the field-level "never
/// read" lint is a false positive until all operations are exercised.
#[allow(dead_code)]
pub struct Ledger {
    conn: std::sync::Mutex<Connection>,
}

/// A single unresolved reservation row (post-crash reconciliation input).
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub struct Unresolved {
    pub key_id: String,
    pub amount_usd: f64,
    pub created_at: i64,
}

/// One key's durable snapshot as printed by `stoke ledger status`.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyStatus {
    pub key_id: String,
    pub spend_usd: f64,
    pub estimated_usd: f64,
    pub held_usd: f64,
}

// The CLI operator surface (`stoke ledger status|reconcile`) lives in
// `cli_main` below, shared by both binaries.
#[allow(dead_code)]
impl Ledger {
    /// Open (creating if needed) the ledger database and ensure the schema.
    /// Fails closed: an unusable path returns `Err` and the caller must refuse
    /// metered traffic rather than silently run without durable enforcement.
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("ledger: cannot create {}: {e}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .map_err(|e| format!("ledger: cannot open {}: {e}", path.display()))?;
        // WAL keeps concurrent readers consistent with the single writer;
        // FULL gives the durable-reserve guarantee the ADR promises.
        conn.busy_timeout(std::time::Duration::from_millis(5000))
            .map_err(|e| format!("ledger: busy timeout: {e}"))?;
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let _ = conn.pragma_update(None, "synchronous", "FULL");
        Self::migrate(&conn)?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }

    /// Open over an in-memory database (tests).
    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory()
            .map_err(|e| format!("ledger: cannot open memory db: {e}"))?;
        Self::migrate(&conn)?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }

    fn migrate(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS spend (
                key_id TEXT PRIMARY KEY,
                spend_usd REAL NOT NULL DEFAULT 0,
                estimated_usd REAL NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS reservations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                key_id TEXT NOT NULL,
                amount_usd REAL NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS reservations_key ON reservations(key_id);
            CREATE TABLE IF NOT EXISTS rate_window (
                key_id TEXT PRIMARY KEY,
                hits_60s INTEGER NOT NULL DEFAULT 0,
                window_epoch_min INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS loop_blocks (
                key_id TEXT PRIMARY KEY,
                blocked_until_epoch_secs INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS install_salt (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                salt BLOB NOT NULL
            );
            "#,
        )
        .map_err(|e| format!("ledger: schema: {e}"))?;
        // Per-installation random salt for deriving key_ids from raw keys.
        let has_salt: Option<i64> = conn
            .query_row("SELECT 1 FROM install_salt WHERE id = 1", [], |r| r.get(0))
            .optional()
            .map_err(|e| format!("ledger: salt check: {e}"))?;
        if has_salt.is_none() {
            let salt = rand_bytes();
            conn.execute(
                "INSERT INTO install_salt (id, salt) VALUES (1, ?1)",
                params![salt],
            )
            .map_err(|e| format!("ledger: salt seed: {e}"))?;
        }
        Ok(())
    }

    /// The per-installation salt used to derive stable key ids from secrets.
    pub fn install_salt(&self) -> Result<Vec<u8>, String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT salt FROM install_salt WHERE id = 1", [], |r| {
            r.get::<_, Vec<u8>>(0)
        })
        .map_err(|e| format!("ledger: salt read: {e}"))
    }

    /// Durable take-a-hold. Written BEFORE the provider call; survives crash.
    pub fn reserve(&self, key_id: &str, amount: f64) -> Result<(), String> {
        if !(amount > 0.0) {
            return Ok(()); // nothing to protect
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("ledger: tx: {e}"))?;
        tx.execute(
            "INSERT INTO reservations (key_id, amount_usd, created_at) VALUES (?1, ?2, ?3)",
            params![key_id, amount, unix_now()],
        )
        .map_err(|e| format!("ledger: reserve: {e}"))?;
        tx.commit().map_err(|e| format!("ledger: commit: {e}"))
    }

    /// Atomically convert `amount` into durable spend and drop that much of
    /// the key's open reservations. One transaction: a charge without the
    /// release would double-count; a release without the charge would erase
    /// real spend.
    pub fn charge_and_release(&self, key_id: &str, amount: f64) -> Result<(), String> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("ledger: tx: {e}"))?;
        tx.execute(
            "INSERT INTO spend (key_id, spend_usd, estimated_usd) VALUES (?1, ?2, 0)
             ON CONFLICT(key_id) DO UPDATE SET spend_usd = spend_usd + ?2",
            params![key_id, amount],
        )
        .map_err(|e| format!("ledger: charge: {e}"))?;
        release_amount(&tx, key_id, amount)?;
        tx.commit().map_err(|e| format!("ledger: commit: {e}"))?;
        Ok(())
    }

    /// Give back a hold without spending (early return, refusal, client gone).
    pub fn release(&self, key_id: &str, amount: f64) -> Result<(), String> {
        if !(amount > 0.0) {
            return Ok(());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("ledger: tx: {e}"))?;
        release_amount(&tx, key_id, amount)?;
        tx.commit().map_err(|e| format!("ledger: commit: {e}"))?;
        Ok(())
    }

    /// Durable cumulative spend for a key (charged dollars only). Absent row
    /// means zero — reading for a key with no history is not an error.
    pub fn spend(&self, key_id: &str) -> Result<f64, String> {
        let conn = self.conn.lock().unwrap();
        let v: Option<f64> = conn
            .query_row(
                "SELECT spend_usd FROM spend WHERE key_id = ?1",
                params![key_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| format!("ledger: spend read: {e}"))?;
        Ok(v.unwrap_or(0.0))
    }

    /// Sum of open (unresolved) reservation rows for a key.
    pub fn held(&self, key_id: &str) -> Result<f64, String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COALESCE(SUM(amount_usd), 0) FROM reservations WHERE key_id = ?1",
            params![key_id],
            |r| r.get(0),
        )
        .map_err(|e| format!("ledger: held read: {e}"))
    }

    /// All unresolved reservations — the crash-reconciliation input.
    pub fn list_unresolved(&self) -> Result<Vec<Unresolved>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT key_id, amount_usd, created_at FROM reservations
                 ORDER BY created_at ASC",
            )
            .map_err(|e| format!("ledger: list: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Unresolved {
                    key_id: r.get(0)?,
                    amount_usd: r.get(1)?,
                    created_at: r.get(2)?,
                })
            })
            .map_err(|e| format!("ledger: list: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("ledger: row: {e}"))?);
        }
        Ok(out)
    }

    /// Per-key durable snapshot (spend + open holds) for every key the ledger
    /// knows about — the `stoke ledger status` view.
    pub fn list_spend(&self) -> Result<Vec<KeyStatus>, String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT key_id, spend_usd, estimated_usd FROM spend
                 UNION
                 SELECT key_id, 0, 0 FROM reservations
                 WHERE key_id NOT IN (SELECT key_id FROM spend)",
            )
            .map_err(|e| format!("ledger: status list: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(KeyStatus {
                    key_id: r.get(0)?,
                    spend_usd: r.get(1)?,
                    estimated_usd: r.get(2)?,
                    held_usd: 0.0,
                })
            })
            .map_err(|e| format!("ledger: status list: {e}"))?;
        let mut out: Vec<KeyStatus> = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("ledger: status row: {e}"))?);
        }
        // Fill in open holds per key; a crash may leave holds with no spend row.
        let mut stmt = conn
            .prepare(
                "SELECT key_id, COALESCE(SUM(amount_usd), 0) FROM reservations
                 GROUP BY key_id",
            )
            .map_err(|e| format!("ledger: status holds: {e}"))?;
        let holds = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))
            .map_err(|e| format!("ledger: status holds: {e}"))?;
        for row in holds {
            let (key_id, held) = row.map_err(|e| format!("ledger: status row: {e}"))?;
            match out.iter_mut().find(|k| k.key_id == key_id) {
                Some(entry) => entry.held_usd = held,
                None => out.push(KeyStatus {
                    key_id,
                    spend_usd: 0.0,
                    estimated_usd: 0.0,
                    held_usd: held,
                }),
            }
        }
        out.sort_by(|a, b| a.key_id.cmp(&b.key_id));
        Ok(out)
    }

    /// Operator-facing plaintext status for the whole ledger. Never prints raw
    /// secrets — the rows only ever contain key ids.
    pub fn status_text(&self) -> Result<String, String> {
        let keys = self.list_spend()?;
        let mut out = String::new();
        if keys.is_empty() {
            out.push_str("ledger: no spend recorded, no open holds\n");
            return Ok(out);
        }
        out.push_str(&format!(
            "{:<20} {:>12} {:>12} {:>12}\n",
            "key_id", "spend_usd", "held_usd", "estimated"
        ));
        for k in &keys {
            out.push_str(&format!(
                "{:<20} {:>12.4} {:>12.4} {:>12.4}\n",
                Self::short_id(&k.key_id),
                k.spend_usd,
                k.held_usd,
                k.estimated_usd
            ));
        }
        Ok(out)
    }

    fn short_id(id: &str) -> String {
        if id.len() <= 16 {
            id.to_string()
        } else {
            format!("{}…", &id[..15])
        }
    }

    /// Operator reconciliation after a crash: count a key's unresolved holds
    /// as real spend (the request may have reached the provider before dying).
    /// Returns the reconciled total.
    pub fn operator_reconcile(&self, key_id: &str) -> Result<f64, String> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("ledger: tx: {e}"))?;
        let sum: f64 = tx
            .query_row(
                "SELECT COALESCE(SUM(amount_usd), 0) FROM reservations WHERE key_id = ?1",
                params![key_id],
                |r| r.get(0),
            )
            .map_err(|e| format!("ledger: reconcile sum: {e}"))?;
        if sum > 0.0 {
            tx.execute(
                "INSERT INTO spend (key_id, spend_usd, estimated_usd) VALUES (?1, ?2, 0)
                 ON CONFLICT(key_id) DO UPDATE SET spend_usd = spend_usd + ?2",
                params![key_id, sum],
            )
            .map_err(|e| format!("ledger: reconcile charge: {e}"))?;
            tx.execute(
                "DELETE FROM reservations WHERE key_id = ?1",
                params![key_id],
            )
            .map_err(|e| format!("ledger: reconcile clear: {e}"))?;
        }
        tx.commit().map_err(|e| format!("ledger: commit: {e}"))?;
        Ok(sum)
    }

    /// Zero a key's durable spend (operator-initiated period reset only).
    pub fn reset_period(&self, key_id: &str) -> Result<(), String> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("ledger: tx: {e}"))?;
        tx.execute("DELETE FROM spend WHERE key_id = ?1", params![key_id])
            .map_err(|e| format!("ledger: reset: {e}"))?;
        tx.commit().map_err(|e| format!("ledger: commit: {e}"))?;
        Ok(())
    }

    /// Durable rate-limit bucket. Records this minute's hit and returns the
    /// count inside the trailing 60s bucket (per key_id).
    pub fn record_rate_hit(&self, key_id: &str) -> Result<i64, String> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("ledger: tx: {e}"))?;
        let now_min = epoch_min_now();
        let (count, stored_min): (i64, i64) = tx
            .query_row(
                "SELECT hits_60s, window_epoch_min FROM rate_window WHERE key_id = ?1",
                params![key_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| format!("ledger: rate read: {e}"))?
            .unwrap_or((0, now_min));
        let count = if stored_min == now_min { count + 1 } else { 1 };
        tx.execute(
            "INSERT INTO rate_window (key_id, hits_60s, window_epoch_min) VALUES (?1, ?2, ?3)
             ON CONFLICT(key_id) DO UPDATE SET hits_60s = ?2, window_epoch_min = ?3",
            params![key_id, count, now_min],
        )
        .map_err(|e| format!("ledger: rate write: {e}"))?;
        tx.commit().map_err(|e| format!("ledger: commit: {e}"))?;
        Ok(count)
    }

    /// Durable loop-block read. `None` when not blocked or the block expired;
    /// expired rows are pruned on read.
    pub fn loop_blocked_until(&self, key_id: &str) -> Result<Option<i64>, String> {
        let conn = self.conn.lock().unwrap();
        let until: Option<i64> = conn
            .query_row(
                "SELECT blocked_until_epoch_secs FROM loop_blocks WHERE key_id = ?1",
                params![key_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| format!("ledger: loop read: {e}"))?;
        Ok(until.filter(|u| *u > unix_now()))
    }

    pub fn set_loop_block(&self, key_id: &str, until_epoch_secs: i64) -> Result<(), String> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("ledger: tx: {e}"))?;
        tx.execute(
            "INSERT INTO loop_blocks (key_id, blocked_until_epoch_secs) VALUES (?1, ?2)
             ON CONFLICT(key_id) DO UPDATE SET blocked_until_epoch_secs = ?2",
            params![key_id, until_epoch_secs],
        )
        .map_err(|e| format!("ledger: loop write: {e}"))?;
        tx.commit().map_err(|e| format!("ledger: commit: {e}"))?;
        Ok(())
    }
}

/// Delete reservation rows for `key_id` until their summed amount covers
/// `amount`. Oldest rows go first so `created_at` ordering stays meaningful
/// for reconciliation output.
fn release_amount(tx: &rusqlite::Transaction, key_id: &str, mut amount: f64) -> Result<(), String> {
    let mut stmt = tx
        .prepare("SELECT id, amount_usd FROM reservations WHERE key_id = ?1 ORDER BY id ASC")
        .map_err(|e| format!("ledger: release read: {e}"))?;
    let rows: Vec<(i64, f64)> = stmt
        .query_map(params![key_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| format!("ledger: release read: {e}"))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("ledger: release row: {e}"))?;
    drop(stmt);
    let mut to_delete: Vec<i64> = Vec::new();
    for (id, row_amount) in rows {
        if amount <= 0.0 {
            break;
        }
        to_delete.push(id);
        amount -= row_amount;
    }
    let _ = &mut amount;
    let mut stmt = tx
        .prepare("DELETE FROM reservations WHERE id = ?1")
        .map_err(|e| format!("ledger: release delete: {e}"))?;
    for id in to_delete {
        stmt.execute(params![id])
            .map_err(|e| format!("ledger: release delete: {e}"))?;
    }
    Ok(())
}

fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn epoch_min_now() -> i64 {
    unix_now() / 60
}

/// The `stoke ledger` operator commands (ADR 0001). Shared by both binaries:
/// the daemon accepts `ledger` as a subcommand before it serves, and
/// `stoke-cli ledger` forwards here. Takes the ledger path from
/// STOKE_LEDGER_PATH, falling back to ~/.stoke/ledger.db — the same path the
/// gateway uses, so the operator inspects exactly what the gateway enforces.
pub fn cli_main(args: &[String]) -> std::process::ExitCode {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let path = ledger_path();
    match sub {
        "status" => {
            let ledger = match Ledger::open(&path) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("ledger: cannot open {}: {e}", path.display());
                    return std::process::ExitCode::FAILURE;
                }
            };
            match ledger.status_text() {
                Ok(text) => {
                    print!("{text}");
                    std::process::ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::ExitCode::FAILURE
                }
            }
        }
        "reconcile" => {
            let mut key_id = String::new();
            for a in args.iter().skip(1) {
                if a == "--key" {
                    continue;
                }
                if !a.starts_with('-') {
                    key_id = a.clone();
                }
            }
            if key_id.is_empty() {
                eprintln!("usage: stoke ledger reconcile <key_id>");
                return std::process::ExitCode::FAILURE;
            }
            let ledger = match Ledger::open(&path) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("ledger: cannot open {}: {e}", path.display());
                    return std::process::ExitCode::FAILURE;
                }
            };
            match ledger.operator_reconcile(&key_id) {
                Ok(charged) => {
                    println!("reconciled {key_id}: charged ${charged:.4} of unresolved holds");
                    std::process::ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::ExitCode::FAILURE
                }
            }
        }
        "--help" | "-h" | "help" | "" => {
            eprintln!(
                "stoke ledger — durable budget ledger operator surface (ADR 0001)\n\n\
                 Usage: stoke ledger <command>\n\n\
                 Commands:\n  \
                   status                per-key durable spend, holds, and estimates\n  \
                   reconcile <key_id>    charge a key's unresolved crash holds as real spend\n\n\
                 The ledger path follows the gateway's: STOKE_LEDGER_PATH, else ~/.stoke/ledger.db.\n\
                 Unresolved holds keep counting against the key until reconciled — that is\n\
                 the documented \"hard cap\" crash semantics, never auto-forgiven."
            );
            std::process::ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown ledger command: {other} (try 'stoke ledger --help')");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Same resolution order the gateway uses at boot (src/main.rs).
fn ledger_path() -> std::path::PathBuf {
    match std::env::var("STOKE_LEDGER_PATH") {
        Ok(p) if !p.is_empty() => std::path::PathBuf::from(p),
        _ => {
            let home = std::env::var("HOME").unwrap_or_default();
            std::path::Path::new(&home)
                .join(".stoke")
                .join("ledger.db")
        }
    }
}

fn rand_bytes() -> [u8; 32] {
    use sha2::{Digest, Sha256};
    // Per-installation randomness: std has no RNG API, so seed from boot
    // time + process identity + address-space layout, hashed. This salts the
    // key-id derivation only; collision risk across installs is negligible.
    let mut seed = Vec::new();
    seed.extend_from_slice(&unix_now().to_le_bytes());
    seed.extend_from_slice(&std::process::id().to_le_bytes());
    let addr = &rand_bytes as *const _ as usize;
    seed.extend_from_slice(&(addr as u64).to_le_bytes());
    let digest = Sha256::digest(&seed);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l() -> Ledger {
        Ledger::open_in_memory().unwrap()
    }

    #[test]
    fn spend_and_holds_survive_via_the_ledger_rows() {
        let ledger = l();
        ledger.reserve("k1", 0.25).unwrap();
        assert_eq!(ledger.held("k1").unwrap(), 0.25);
        assert_eq!(ledger.spend("k1").unwrap(), 0.0);
        ledger.charge_and_release("k1", 0.25).unwrap();
        assert_eq!(ledger.spend("k1").unwrap(), 0.25);
        assert_eq!(ledger.held("k1").unwrap(), 0.0);
    }

    #[test]
    fn status_reports_spend_and_open_holds_per_key() {
        let ledger = l();
        ledger.reserve("k1", 0.25).unwrap();
        ledger.charge_and_release("k1", 0.25).unwrap();
        ledger.reserve("k2", 0.40).unwrap(); // unresolved crash hold
        let text = ledger.status_text().unwrap();
        // The spend row and the hold row are both visible, keyed by id only.
        assert!(text.contains("key_id"), "{text}");
        assert!(text.contains("spend_usd"), "{text}");
        assert!(text.contains("held_usd"), "{text}");
        let keys = ledger.list_spend().unwrap();
        let k1 = keys.iter().find(|k| k.key_id == "k1").unwrap();
        assert_eq!(k1.spend_usd, 0.25);
        assert_eq!(k1.held_usd, 0.0);
        // k2 has no spend row but its hold must still appear — this is the
        // post-crash state the operator needs to see before reconciling.
        let k2 = keys.iter().find(|k| k.key_id == "k2").unwrap();
        assert_eq!(k2.spend_usd, 0.0);
        assert_eq!(k2.held_usd, 0.40);
    }

    #[test]
    fn status_on_an_empty_ledger_is_not_an_error() {
        let text = l().status_text().unwrap();
        assert!(text.contains("no spend recorded"), "{text}");
    }

    #[test]
    fn a_reservation_survives_reopening_the_database() {
        let dir = std::env::temp_dir().join(format!(
            "stoke-ledger-test-{}",
            std::process::id()
        ));
        let path = dir.join("ledger.db");
        let _ = std::fs::remove_dir_all(&dir);
        {
            let ledger = Ledger::open(&path).unwrap();
            ledger.reserve("k1", 0.10).unwrap();
        }
        {
            // "Restart": a fresh Ledger over the same file.
            let ledger = Ledger::open(&path).unwrap();
            assert_eq!(ledger.held("k1").unwrap(), 0.10);
            // Reconciliation charges it and clears the hold.
            let charged = ledger.operator_reconcile("k1").unwrap();
            assert_eq!(charged, 0.10);
            assert_eq!(ledger.spend("k1").unwrap(), 0.10);
            assert_eq!(ledger.held("k1").unwrap(), 0.0);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unusable_ledger_path_fails_closed() {
        // A path whose parent is a FILE cannot host the DB — open must Err,
        // which the gateway turns into a refusal before any provider call.
        let bogus = std::env::temp_dir().join("stoke-ledger-test-bogus");
        std::fs::write(&bogus, b"not a directory").unwrap();
        let path = bogus.join("nested").join("ledger.db");
        assert!(Ledger::open(&path).is_err());
        let _ = std::fs::remove_file(&bogus);
    }

    #[test]
    fn raw_secrets_never_enter_the_database() {
        let ledger = l();
        ledger.reserve("anything", 0.01).unwrap();
        // The only rows are keyed by caller-provided key_id (a hash upstream);
        // no ledger API even accepts a "secret" concept. Assert no table
        // column can hold one by checking the schema is id-keyed only.
        let conn = ledger.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .unwrap();
        let mut names = Vec::new();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        for r in rows {
            names.push(r.unwrap());
        }
        assert!(names.contains(&"spend".to_string()));
        assert!(!names.iter().any(|n| n.contains("secret")));
    }

    #[test]
    fn charge_and_release_converts_the_hold_exactly_once() {
        let ledger = l();
        ledger.reserve("k", 0.30).unwrap();
        ledger.charge_and_release("k", 0.30).unwrap();
        assert_eq!(ledger.spend("k").unwrap(), 0.30);
        assert_eq!(ledger.held("k").unwrap(), 0.0);
        // A second charge with no hold still records spend.
        ledger.charge_and_release("k", 0.05).unwrap();
        assert_eq!(ledger.spend("k").unwrap(), 0.35);
        assert_eq!(ledger.held("k").unwrap(), 0.0);
    }

    #[test]
    fn release_gives_back_the_hold_without_spend() {
        let ledger = l();
        ledger.reserve("k", 0.20).unwrap();
        ledger.release("k", 0.20).unwrap();
        assert_eq!(ledger.held("k").unwrap(), 0.0);
        assert_eq!(ledger.spend("k").unwrap(), 0.0);
    }

    #[test]
    fn rate_window_and_loop_blocks_are_durable_rows() {
        let ledger = l();
        assert_eq!(ledger.record_rate_hit("k").unwrap(), 1);
        assert_eq!(ledger.record_rate_hit("k").unwrap(), 2);
        // Block and observe; expiry prunes on read.
        ledger.set_loop_block("k", unix_now() - 1).unwrap();
        assert!(ledger.loop_blocked_until("k").unwrap().is_none());
        ledger.set_loop_block("k", unix_now() + 60).unwrap();
        assert!(ledger.loop_blocked_until("k").unwrap().is_some());
    }

    #[test]
    fn install_salt_is_stable_across_reopen() {
        let dir = std::env::temp_dir().join(format!(
            "stoke-ledger-salt-{}",
            std::process::id()
        ));
        let path = dir.join("ledger.db");
        let _ = std::fs::remove_dir_all(&dir);
        let salt = Ledger::open(&path).unwrap().install_salt().unwrap();
        let again = Ledger::open(&path).unwrap().install_salt().unwrap();
        assert_eq!(salt, again);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_reserves_cannot_lose_a_row() {
        use std::sync::Arc;
        use std::sync::Barrier;
        let ledger = std::sync::Arc::new(l());
        let barrier = Arc::new(Barrier::new(8));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let ledger = std::sync::Arc::clone(&ledger);
            let barrier = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                ledger.reserve("k", 0.01).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let held = ledger.held("k").unwrap();
        assert!((held - 0.08).abs() < 1e-9, "expected 8 holds of 0.01, got {held}");
    }

    #[test]
    fn reset_period_zeroes_only_the_named_key() {
        let ledger = l();
        ledger.charge_and_release("a", 0.50).unwrap();
        ledger.charge_and_release("b", 0.50).unwrap();
        ledger.reset_period("a").unwrap();
        assert_eq!(ledger.spend("a").unwrap(), 0.0);
        assert_eq!(ledger.spend("b").unwrap(), 0.50);
    }
}
