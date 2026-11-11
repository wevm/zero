//! What the chaos tests share, as Swift's `ChaosHarness`: the fixture
//! database read and written with `psql` behind zero-cache's back, clients
//! of zero-cache or of a [`ChaosProxy`](super::proxy::ChaosProxy), and
//! waits.

use super::{E2e, ids, unique};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use zero_client::client::{
    ConnectionStatus, Deferred, LogLevel, LogSink, MaterializedView, MutationOutcome, Zero,
    ZeroOptions, ZeroStorage,
};
use zero_client::ivm::ResultType;
use zero_client::ttl::Ttl;

/// The fixture database. `ZERO_E2E_PG_DB` overrides its URL.
pub fn pg_url() -> String {
    std::env::var("ZERO_E2E_PG_DB")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:5432/zero_e2e".to_owned())
}

/// Runs `sql` and returns its rows, each with its columns joined by `|`.
pub fn psql(sql: &str) -> Vec<String> {
    let out = std::process::Command::new("psql")
        .args([&pg_url(), "-v", "ON_ERROR_STOP=1", "-qAt", "-c", sql])
        .output()
        .expect("psql runs");
    assert!(
        out.status.success(),
        "psql: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The IDs of the open issues whose ID starts with `prefix`.
pub fn open_issue_ids(prefix: &str) -> BTreeSet<String> {
    psql(&format!(
        "SELECT id FROM issues WHERE NOT closed AND id LIKE '{prefix}%'"
    ))
    .into_iter()
    .collect()
}

/// Every issue whose ID starts with `prefix`, as `id|title|closed`.
pub fn issues(prefix: &str) -> BTreeSet<String> {
    psql(&format!(
        "SELECT id || '|' || title || '|' || closed FROM issues WHERE id LIKE '{prefix}%'"
    ))
    .into_iter()
    .collect()
}

/// The last mutation ID zero-cache recorded for `client_id`.
pub fn last_mutation_id(client_id: &str) -> Option<u64> {
    psql(&format!(
        "SELECT \"lastMutationID\" FROM zero_0.clients WHERE \"clientID\" = '{client_id}'"
    ))
    .first()
    .and_then(|v| v.parse().ok())
}

/// Collects a client's log lines.
#[derive(Default)]
pub struct LogCollector(RefCell<Vec<String>>);

impl LogSink for LogCollector {
    fn log(&self, level: LogLevel, message: &str, _context: &[(String, String)]) {
        self.0
            .borrow_mut()
            .push(format!("[{}] {message}", level.as_str()));
    }
}

impl LogCollector {
    pub fn count(&self, substring: &str) -> usize {
        self.0
            .borrow()
            .iter()
            .filter(|l| l.contains(substring))
            .count()
    }
}

/// One chaos test runs at a time, as Swift's `.serialized` suites: they
/// time pings and reconnects, which other tests' load would skew.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A chaos test's clients and rows. Dropping it deletes the rows whose ID
/// starts with its prefix and its SQLite stores, even when the test fails,
/// so the fixture's open issues stay the ones the walking skeleton expects.
pub struct Chaos {
    pub e: E2e,
    pub prefix: String,
    directory: PathBuf,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

impl Chaos {
    /// `None` without `ZERO_E2E_URL`.
    pub async fn new() -> Option<Chaos> {
        let e = E2e::new()?;
        let serial = SERIAL.lock().await;
        Some(Chaos {
            e,
            prefix: format!("{}-", unique("chaos")),
            directory: std::env::temp_dir().join(unique("zero-chaos")),
            _serial: serial,
        })
    }

    /// zero-cache's `ws://` origin.
    pub fn upstream(&self) -> String {
        self.e.origin.replacen("http://", "ws://", 1)
    }

    /// A client of `cache_url` (zero-cache itself when `None`), on SQLite
    /// or in memory.
    pub fn zero(
        &self,
        cache_url: Option<&str>,
        storage_key: &str,
        sqlite: bool,
        configure: impl FnOnce(&mut ZeroOptions),
    ) -> Zero {
        self.e.zero_with(storage_key, |o| {
            if let Some(url) = cache_url {
                o.cache_url = Some(url.to_owned());
            }
            o.storage = if sqlite {
                ZeroStorage::Sqlite {
                    directory: Some(self.directory.clone()),
                }
            } else {
                ZeroStorage::Memory
            };
            o.log_level = LogLevel::Info;
            configure(o);
        })
    }

    /// A client of zero-cache itself, in memory.
    pub fn direct(&self) -> Zero {
        self.zero(None, &unique("chaos-key"), false, |_| {})
    }

    /// The live `issues.open` of `zero`.
    pub fn open(&self, zero: &Zero) -> MaterializedView {
        zero.materialize(&self.e.open(), &Ttl::default()).unwrap()
    }

    /// The IDs of this test's issues in a view of the open issues.
    pub fn ids(&self, open: &MaterializedView) -> BTreeSet<String> {
        ids(&open.data())
            .into_iter()
            .filter(|id| id.starts_with(&self.prefix))
            .collect()
    }

    /// Inserts `count` open issues `<prefix>seed-<n>` straight into
    /// Postgres, with descriptions of `description_bytes`, and waits until
    /// zero-cache has them.
    pub async fn seed(&self, count: usize, description_bytes: usize) {
        let prefix = &self.prefix;
        psql(&format!(
            "INSERT INTO issues (id, title, description, closed, owner_id, \"createdAt\")
             SELECT '{prefix}seed-' || lpad(n::text, 6, '0'), 'Seeded ' || n,
               repeat('x', {description_bytes}), false, NULL, 1000 + n
             FROM generate_series(1, {count}) AS n"
        ));
        self.replicated(&format!("{prefix}seed-{count:06}")).await;
    }

    /// Waits until zero-cache has replicated `id`: a client straight to it
    /// sees the issue.
    pub async fn replicated(&self, id: &str) {
        let probe = self.direct();
        let issue = probe
            .materialize(&self.e.by_id(id), &Ttl::default())
            .unwrap();
        wait_for(
            &format!("{id} to replicate"),
            Duration::from_secs(30),
            || !super::rows(&issue.data()).is_empty(),
        )
        .await;
        probe.close();
    }
}

impl Drop for Chaos {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
        psql(&format!(
            "DELETE FROM issues WHERE id LIKE '{}%'",
            self.prefix
        ));
    }
}

pub fn connected(zero: &Zero) -> bool {
    zero.connection().state().status() == ConnectionStatus::Connected
}

pub fn complete(view: &MaterializedView) -> bool {
    view.result_type() == ResultType::Complete
}

/// Waits until `condition` holds; fails the test after `timeout`.
pub async fn wait_for(what: &str, timeout: Duration, condition: impl FnMut() -> bool) {
    assert!(
        within(timeout, condition).await,
        "timed out after {timeout:?} waiting for {what}"
    );
}

/// Whether `condition` holds within `timeout`.
pub async fn within(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while !condition() {
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    true
}

/// The outcome of `deferred` within `timeout`, or `None`.
pub async fn settled(
    deferred: &Deferred<MutationOutcome>,
    timeout: Duration,
) -> Option<MutationOutcome> {
    within(timeout, || deferred.is_settled()).await;
    deferred.result().and_then(Result::ok)
}

/// Runs a chaos test's body on the client's thread, within `limit`.
pub async fn run(limit: Duration, body: impl Future<Output = ()>) {
    tokio::time::timeout(limit, tokio::task::LocalSet::new().run_until(body))
        .await
        .unwrap_or_else(|_| panic!("the test finished within {limit:?}"));
}

/// `got` and `want` differ: a short difference.
pub fn difference(got: &BTreeSet<String>, want: &BTreeSet<String>) -> String {
    let missing: Vec<_> = want.difference(got).take(5).collect();
    let extra: Vec<_> = got.difference(want).take(5).collect();
    format!(
        "{} of {}; missing {}, {missing:?}; extra {}, {extra:?}",
        got.len(),
        want.len(),
        want.difference(got).count(),
        got.difference(want).count()
    )
}

/// `logs` as a client's log sink.
pub fn sink(logs: &Rc<LogCollector>) -> Rc<dyn LogSink> {
    logs.clone()
}
