//! Random chaos against the real zero-cache, as Swift's `ChaosFuzzTests`.
//! Each seed runs two clients, on SQLite stores of their own, through a
//! [`ChaosProxy`], and takes 40 random steps: mutations, connections cut,
//! refused, stalled, or cut partway through what the server sends, writes
//! to Postgres behind the clients' back, and relaunches on the same store.
//! Then the network heals, and the oracle is convergence: Postgres holds
//! every mutation once, each client's last mutation ID counts all of its
//! mutations, and both clients see what Postgres holds.
//!
//! PRs run `ZERO_CHAOS_SEEDS` seeds (2 by default) from
//! `ZERO_CHAOS_FIRST_SEED` (0); the nightly workflow runs more. A seed takes
//! the same steps as in Swift. To replay one:
//! `ZERO_CHAOS_SEED=<seed> ZERO_E2E_URL=... cargo test -p zero-client
//! --features tokio-transport,sqlite --test client_e2e fuzz -- --nocapture`.

use super::chaos::{
    Chaos, complete, connected, difference, issues, last_mutation_id, open_issue_ids, psql, run,
    within,
};
use super::proxy::{Accept, ChaosProxy, Knobs};
use super::{close, create, unique};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};
use zero_client::client::{MaterializedView, Zero};

/// SplitMix64, drawn from as Swift's `random(in:using:)` draws: the same
/// steps for a seed in both clients.
struct ChaosRandom(u64);

impl ChaosRandom {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// `next(upperBound:)` in Swift's standard library: Lemire's method.
    fn below(&mut self, upper: u64) -> u64 {
        let mut m = u128::from(self.next()) * u128::from(upper);
        if (m as u64) < upper {
            let t = upper.wrapping_neg() % upper;
            while (m as u64) < t {
                m = u128::from(self.next()) * u128::from(upper);
            }
        }
        (m >> 64) as u64
    }

    /// `Int.random(in: low..<high, using:)`.
    fn range(&mut self, low: u64, high: u64) -> u64 {
        low + self.below(high - low)
    }

    /// `Int.random(in: low...high, using:)`.
    fn closed(&mut self, low: u64, high: u64) -> u64 {
        self.range(low, high + 1)
    }

    /// An index into a collection of `len`.
    fn index(&mut self, len: usize) -> usize {
        self.range(0, len as u64) as usize
    }
}

/// A client and the relaunches on its store.
struct Lineage {
    storage_key: String,
    zero: Zero,
    open: MaterializedView,
    /// What the client now running created and hasn't closed: only it may
    /// close them, since its mutations apply in order.
    closable: Vec<String>,
}

/// One seed's clients, proxy, and model of what Postgres should end up
/// with.
struct ChaosRun<'a> {
    chaos: &'a Chaos,
    rng: ChaosRandom,
    proxy: ChaosProxy,
    lineages: Vec<Lineage>,
    /// The open issues Postgres should end with.
    expected_open: BTreeSet<String>,
    created: usize,
    /// Mutations made by each client ID.
    mutations: BTreeMap<String, u64>,
    /// Issues written straight to Postgres and still open.
    written: Vec<String>,
    steps: Vec<String>,
}

impl<'a> ChaosRun<'a> {
    fn new(chaos: &'a Chaos, seed: u64) -> ChaosRun<'a> {
        ChaosRun {
            chaos,
            rng: ChaosRandom(seed),
            proxy: ChaosProxy::new(&chaos.upstream()),
            lineages: Vec::new(),
            expected_open: BTreeSet::new(),
            created: 0,
            mutations: BTreeMap::new(),
            written: Vec::new(),
            steps: Vec::new(),
        }
    }

    fn zero(&self, storage_key: &str) -> Zero {
        self.chaos
            .zero(Some(&self.proxy.cache_url()), storage_key, true, |o| {
                o.ping_timeout_ms = 2000.0
            })
    }

    fn lineage(&self, storage_key: String) -> Lineage {
        let zero = self.zero(&storage_key);
        let open = self.chaos.open(&zero);
        Lineage {
            storage_key,
            zero,
            open,
            closable: Vec::new(),
        }
    }

    /// The problems at the end; `None` when everything converged.
    async fn run(mut self) -> Option<String> {
        for _ in 0..2 {
            let l = self.lineage(unique("chaos-key"));
            self.lineages.push(l);
        }
        for l in &self.lineages {
            if !within(Duration::from_secs(30), || connected(&l.zero)).await {
                return Some(format!("{} didn't connect", l.zero.client_id()));
            }
        }
        for n in 0..40 {
            let step = self.step(n).await;
            self.steps.push(format!("{n}: {step}"));
            let ms = self.rng.closed(0, 100);
            sleep_ms(ms).await;
        }
        self.proxy.set(|k| *k = Knobs::default());
        let problems = self.converge().await;
        for l in &self.lineages {
            l.zero.close();
        }
        if problems.is_empty() {
            return None;
        }
        let last = self.steps[self.steps.len().saturating_sub(8)..].join(" | ");
        Some(format!("{} (last steps: {last})", problems.join("; ")))
    }

    fn count(&mut self, i: usize) {
        let id = self.lineages[i].zero.client_id().to_owned();
        *self.mutations.entry(id).or_default() += 1;
    }

    async fn step(&mut self, n: usize) -> String {
        let i = self.rng.index(self.lineages.len());
        let prefix = &self.chaos.prefix;
        match self.rng.range(0, 100) {
            0..35 => {
                let id = format!("{prefix}c{n}");
                create(&self.lineages[i].zero, &id, &format!("Chaos {n}"), None);
                self.count(i);
                self.lineages[i].closable.push(id.clone());
                self.expected_open.insert(id.clone());
                self.created += 1;
                format!("client {i} creates {id}")
            }
            35..45 => {
                if self.lineages[i].closable.is_empty() {
                    return format!("client {i} has nothing to close");
                }
                let at = self.rng.index(self.lineages[i].closable.len());
                let id = self.lineages[i].closable.remove(at);
                close(&self.lineages[i].zero, &id);
                self.count(i);
                self.expected_open.remove(&id);
                format!("client {i} closes {id}")
            }
            45..53 => {
                self.proxy.cut_all();
                "cut every connection".to_owned()
            }
            53..58 => {
                let ms = self.rng.closed(100, 1500);
                self.proxy.set(|k| k.accept = Accept::Refuse);
                self.proxy.cut_all();
                sleep_ms(ms).await;
                self.proxy.set(|k| k.accept = Accept::Pass);
                format!("down for {ms} ms")
            }
            58..63 => {
                let ms = self.rng.closed(200, 3000);
                self.proxy.set(|k| k.downstream_rate = Some(0));
                sleep_ms(ms).await;
                self.proxy.set(|k| k.downstream_rate = None);
                format!("server stalled for {ms} ms")
            }
            63..68 => {
                let bytes = self.rng.closed(500, 50_000) as usize;
                let ms = self.rng.closed(200, 1500);
                self.proxy
                    .set(|k| k.cut_after_downstream_bytes = Some(bytes));
                sleep_ms(ms).await;
                self.proxy.set(|k| k.cut_after_downstream_bytes = None);
                format!("cuts after {bytes} bytes for {ms} ms")
            }
            68..76 => {
                let id = format!("{prefix}pg{n}");
                psql(&format!(
                    "INSERT INTO issues (id, title, description, closed, owner_id, \"createdAt\")
                     VALUES ('{id}', 'Written {n}', '', false, NULL, 2000)"
                ));
                self.written.push(id.clone());
                self.expected_open.insert(id.clone());
                self.created += 1;
                format!("Postgres inserts {id}")
            }
            76..82 => {
                if self.written.is_empty() {
                    return "Postgres has nothing to close".to_owned();
                }
                let at = self.rng.index(self.written.len());
                let id = self.written.remove(at);
                psql(&format!(
                    "UPDATE issues SET closed = true WHERE id = '{id}'"
                ));
                self.expected_open.remove(&id);
                format!("Postgres closes {id}")
            }
            82..90 => {
                self.lineages[i].zero.close();
                let key = self.lineages[i].storage_key.clone();
                self.lineages[i] = self.lineage(key);
                format!("client {i} relaunches")
            }
            _ => {
                let ms = self.rng.closed(0, 500);
                sleep_ms(ms).await;
                format!("wait {ms} ms")
            }
        }
    }

    /// Waits for the clients and Postgres to settle, then compares them
    /// with the model.
    async fn converge(&self) -> Vec<String> {
        let prefix = &self.chaos.prefix;
        let mut problems = Vec::new();
        // Every mutation pushed first: until then, a create closed in the
        // same run leaves the open issues right but is missing from
        // Postgres.
        for (client_id, &count) in &self.mutations {
            let ok = within(Duration::from_secs(30), || {
                last_mutation_id(client_id) == Some(count)
            })
            .await;
            if !ok {
                let last = last_mutation_id(client_id).map_or("none".to_owned(), |l| l.to_string());
                problems.push(format!(
                    "client {client_id} made {count} mutations; its last mutation ID is {last}"
                ));
            }
        }
        let in_postgres = within(Duration::from_secs(30), || {
            open_issue_ids(prefix) == self.expected_open && issues(prefix).len() == self.created
        })
        .await;
        if !in_postgres {
            let open = open_issue_ids(prefix);
            if open != self.expected_open {
                problems.push(format!(
                    "Postgres's open issues: {}",
                    difference(&open, &self.expected_open)
                ));
            }
            let all = issues(prefix).len();
            if all != self.created {
                problems.push(format!(
                    "{all} issues in Postgres, {} created",
                    self.created
                ));
            }
        }
        for (i, l) in self.lineages.iter().enumerate() {
            let ok = within(Duration::from_secs(60), || {
                complete(&l.open) && self.chaos.ids(&l.open) == self.expected_open
            })
            .await;
            if !ok {
                problems.push(format!(
                    "client {i} ({:?}) sees {}",
                    l.zero.connection().state().status(),
                    difference(&self.chaos.ids(&l.open), &self.expected_open)
                ));
            }
        }
        problems
    }
}

async fn sleep_ms(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

fn env_seed(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

#[tokio::test]
async fn random_seeds() {
    let seeds = match env_seed("ZERO_CHAOS_SEED") {
        Some(only) => only..only + 1,
        None => {
            let first = env_seed("ZERO_CHAOS_FIRST_SEED").unwrap_or(0);
            first..first + env_seed("ZERO_CHAOS_SEEDS").unwrap_or(2)
        }
    };
    let mut failures = Vec::new();
    for seed in seeds {
        // Each seed's rows and stores apart, and cleaned up after it.
        let Some(chaos) = Chaos::new().await else {
            return;
        };
        let start = Instant::now();
        let mut failure = None;
        run(Duration::from_secs(600), async {
            failure = ChaosRun::new(&chaos, seed).run().await;
        })
        .await;
        println!(
            "[chaos fuzz] seed {seed}: {} in {:?}",
            failure.as_deref().unwrap_or("converged"),
            start.elapsed()
        );
        if let Some(f) = failure {
            failures.push(format!("seed {seed}: {f}"));
            if failures.len() == 3 {
                break;
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
