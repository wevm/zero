//! Times benchmarks the way `packages/shared/src/bench.ts` does: many
//! samples over a time budget, reported as the median time per call with
//! the 75th and 99th percentiles (`Harness.swift` in the Swift client).

use std::time::Instant;
use zero_client::json::{Object, Value};

pub struct Bench {
    budget: f64,
    filters: Vec<String>,
    results: Vec<Object>,
    group: String,
    epoch: Instant,
}

impl Bench {
    /// Runs each benchmark for `budget` seconds after a warm-up, and only
    /// those whose `group > name` contains one of `filters` (any case).
    pub fn new(budget: f64, filters: Vec<String>) -> Self {
        Bench {
            budget,
            filters: filters.into_iter().map(|f| f.to_lowercase()).collect(),
            results: Vec::new(),
            group: String::new(),
            epoch: Instant::now(),
        }
    }

    /// Names the benchmarks `body` defines `"<name> > …"`, and skips them
    /// (and their setup) unless a filter's part before ` > ` matches `name`.
    pub fn group(&mut self, name: &str, body: impl FnOnce(&mut Bench)) {
        let lower = name.to_lowercase();
        let wanted = self.filters.is_empty()
            || self
                .filters
                .iter()
                .any(|f| lower.contains(f.split(" > ").next().unwrap_or_default()));
        if !wanted {
            return;
        }
        self.group = name.to_owned();
        println!("\n{name}");
        body(self);
        self.group.clear();
    }

    /// Whether the group's benchmark `name` runs, for skipping its setup.
    pub fn wants(&self, name: &str) -> bool {
        self.selected(&format!("{} > {name}", self.group))
    }

    fn selected(&self, name: &str) -> bool {
        let name = name.to_lowercase();
        self.filters.is_empty() || self.filters.iter().any(|f| name.contains(f.as_str()))
    }

    /// Times `body`, called back to back in batches long enough for the
    /// clock to resolve.
    pub fn run(&mut self, name: &str, mut body: impl FnMut()) {
        let full = format!("{} > {name}", self.group);
        if !self.selected(&full) {
            return;
        }
        let mut batch = 1u32;
        while batch < 1 << 20 {
            let t = self.time(|| (0..batch).for_each(|_| body()));
            if t >= 50_000.0 {
                break;
            }
            batch *= 2;
        }
        self.record(full, |b| {
            b.time(|| (0..batch).for_each(|_| body())) / f64::from(batch)
        });
    }

    /// Times `body` with what `setup` returns, made fresh for each call and
    /// not timed, nor is dropping it.
    pub fn run_with<S>(
        &mut self,
        name: &str,
        mut setup: impl FnMut() -> S,
        mut body: impl FnMut(&mut S),
    ) {
        let full = format!("{} > {name}", self.group);
        if !self.selected(&full) {
            return;
        }
        self.record(full, |b| {
            let mut state = setup();
            let t = b.time(|| body(&mut state));
            drop(state);
            t
        });
    }

    fn record(&mut self, name: String, mut sample: impl FnMut(&Bench) -> f64) {
        // Warm up for a tenth of the budget.
        let start = self.now();
        while self.now() - start < self.budget * 1e8 {
            sample(self);
        }
        let mut samples = Vec::new();
        let measure = self.now();
        while samples.len() < 10
            || (self.now() - measure < self.budget * 1e9 && samples.len() < 1_000_000)
        {
            samples.push(sample(self));
        }
        samples.sort_by(f64::total_cmp);
        let n = samples.len();
        let (median, p75, p99, min) = (
            samples[n / 2],
            samples[n * 3 / 4],
            samples[n * 99 / 100],
            samples[0],
        );
        println!(
            "  {} {:>10}/iter  p75 {:>10}  p99 {:>10}  min {:>10}",
            self.label(&name),
            format(median),
            format(p75),
            format(p99),
            format(min)
        );
        let mut result = Object::new();
        result.insert("name", Value::from(name));
        for (key, value) in [
            ("median", median),
            ("p75", p75),
            ("p99", p99),
            ("min", min),
            ("samples", n as f64),
        ] {
            result.insert(key, Value::from(value));
        }
        self.results.push(result);
    }

    /// Records a measured amount of memory, if the platform measures it.
    pub fn record_bytes(&mut self, name: &str, bytes: Option<usize>) {
        let full = format!("{} > {name}", self.group);
        if !self.selected(&full) {
            return;
        }
        let shown = bytes.map_or_else(|| "n/a".to_owned(), |b| format!("{:.1} MB", b as f64 / 1e6));
        println!("  {} {shown:>10}", self.label(&full));
        let mut result = Object::new();
        result.insert("name", Value::from(full));
        result.insert(
            "bytes",
            bytes.map_or(Value::Null, |b| Value::from(b as f64)),
        );
        self.results.push(result);
    }

    /// The results, as `--json` writes them.
    pub fn results(&self) -> Value {
        Value::Array(self.results.iter().cloned().map(Value::from).collect())
    }

    fn label(&self, name: &str) -> String {
        let short = name.get(self.group.len() + 3..).unwrap_or(name);
        format!("{short:<56}")
    }

    fn time(&self, body: impl FnOnce()) -> f64 {
        let start = self.now();
        body();
        self.now() - start
    }

    /// Nanoseconds on the monotonic clock.
    fn now(&self) -> f64 {
        self.epoch.elapsed().as_nanos() as f64
    }
}

fn format(ns: f64) -> String {
    if ns < 1e3 {
        format!("{ns:.2} ns")
    } else if ns < 1e6 {
        format!("{:.2} µs", ns / 1e3)
    } else if ns < 1e9 {
        format!("{:.2} ms", ns / 1e6)
    } else {
        format!("{:.2} s", ns / 1e9)
    }
}

/// Checks a benchmark computed the right thing.
pub fn check(condition: bool, message: &str) {
    if !condition {
        eprintln!("check failed: {message}");
        std::process::exit(1);
    }
}

/// The process's resident memory in bytes, where the platform reports it
/// without a dependency (`VmRSS` on Linux).
pub fn resident_memory() -> Option<usize> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kb: usize = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}
