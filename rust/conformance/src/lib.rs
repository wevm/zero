//! Replays the conformance vectors that `packages/zero-conformance` generates
//! from the JS client against `zero-client`, and reports, file by file, how
//! many cases match JS.
//!
//! Every vector file is reported. A file no suite replays yet is listed as
//! not implemented, so a report never reads as more coverage than it has.
//! Each case is checked on its own: a failing or panicking case is counted
//! and the rest still run. A case that matches JS only under one of the
//! documented exceptions (`rust/PARITY.md`) is counted apart from the cases
//! that match exactly.

mod case;
mod suites;

pub use case::{Case, Outcome};

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use zero_client::json::{self, Value};

/// The vector format this runner reads.
pub const FORMAT_VERSION: f64 = 1.0;

/// Where a suite's vectors come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    /// `packages/zero-conformance/vectors`, checked in.
    Checked,
    /// `packages/zero-conformance/generated`, the seeded fuzz corpora that
    /// `pnpm --filter zero-conformance run test` writes.
    Generated,
}

/// A check of one case: `Ok` with how it matched, or `Err` with how it
/// differs from JS. `file` is the whole vector file, for its `shared`
/// fixtures and for cases checked against the others.
pub type Check = fn(case: &Value, file: &Value) -> Result<Outcome, String>;

/// The check for each case of one vector file.
#[derive(Clone, Copy)]
pub struct Suite {
    pub file: &'static str,
    /// How this suite replays the file when several do so apart, such as
    /// `sqlite` for the client scenarios on SQLite storage: each variant
    /// has a row of its own. `None` for the plain replay.
    pub variant: Option<&'static str>,
    pub source: Source,
    pub check: Check,
}

/// What a run found for one vector file.
#[derive(Debug)]
pub struct FileReport {
    pub file: String,
    pub source: Source,
    /// The suite's variant, such as `sqlite`.
    pub variant: Option<&'static str>,
    pub status: Status,
    pub cases: usize,
    pub passed: usize,
    /// Cases that match under a documented exception, by exception.
    pub exceptions: BTreeMap<&'static str, usize>,
    pub failed: usize,
    /// The first few failures' messages.
    pub failures: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Replayed; see the counts.
    Replayed,
    /// No suite replays this file yet.
    NotImplemented,
    /// The suite's file isn't there (generated corpora not written yet).
    Missing,
    /// The file couldn't be read or parsed.
    Unreadable(String),
}

/// How many failure messages a report keeps per file.
const KEPT_FAILURES: usize = 5;

#[derive(Debug)]
pub struct Report {
    pub files: Vec<FileReport>,
}

impl Report {
    pub fn failed(&self) -> usize {
        self.files.iter().map(|f| f.failed).sum()
    }

    /// Whether every case of every file that was replayed matches JS.
    pub fn ok(&self) -> bool {
        self.files
            .iter()
            .all(|f| f.failed == 0 && !matches!(f.status, Status::Unreadable(_)))
    }

    /// Suites whose files are missing, from `source`.
    pub fn missing(&self, source: Source) -> Vec<&str> {
        self.files
            .iter()
            .filter(|f| f.source == source && f.status == Status::Missing)
            .map(|f| f.file.as_str())
            .collect()
    }

    /// The report as a Markdown table, with totals and the failures kept.
    pub fn markdown(&self) -> String {
        let mut out = String::new();
        let mut totals = [0usize; 4];
        out.push_str("| Vectors | Cases | Match JS | Match with exception | Fail | Status |\n");
        out.push_str("|---|---:|---:|---:|---:|---|\n");
        for f in &self.files {
            let exceptions: usize = f.exceptions.values().sum();
            let dir = match f.source {
                Source::Checked => "vectors",
                Source::Generated => "generated",
            };
            let status = match &f.status {
                Status::Replayed if f.failed == 0 => "pass".to_owned(),
                Status::Replayed => "**FAIL**".to_owned(),
                Status::NotImplemented => "not implemented".to_owned(),
                Status::Missing => "missing".to_owned(),
                Status::Unreadable(e) => format!("**unreadable**: {e}"),
            };
            let replayed = f.status == Status::Replayed;
            let n = |v: usize| {
                if replayed {
                    v.to_string()
                } else {
                    "–".to_owned()
                }
            };
            let variant = f.variant.map(|v| format!(" ({v})")).unwrap_or_default();
            let _ = writeln!(
                out,
                "| `{dir}/{}.json`{variant} | {} | {} | {} | {} | {status} |",
                f.file,
                if f.status == Status::Missing {
                    "–".to_owned()
                } else {
                    f.cases.to_string()
                },
                n(f.passed),
                n(exceptions),
                n(f.failed),
            );
            totals[0] += f.cases;
            if replayed {
                totals[1] += f.passed;
                totals[2] += exceptions;
                totals[3] += f.failed;
            }
        }
        let replayed: usize = self
            .files
            .iter()
            .filter(|f| f.status == Status::Replayed)
            .map(|f| f.cases)
            .sum();
        let _ = writeln!(
            out,
            "| **Total** | {} | {} | {} | {} | {} of {} cases replayed |",
            totals[0], totals[1], totals[2], totals[3], replayed, totals[0]
        );
        let mut exceptions: BTreeMap<&str, usize> = BTreeMap::new();
        for f in &self.files {
            for (e, n) in &f.exceptions {
                *exceptions.entry(e).or_default() += n;
            }
        }
        if !exceptions.is_empty() {
            out.push_str("\nCases matched under a documented exception (`rust/PARITY.md`):\n\n");
            for (e, n) in exceptions {
                let _ = writeln!(out, "- {e}: {n}");
            }
        }
        let failing: Vec<_> = self.files.iter().filter(|f| f.failed > 0).collect();
        if !failing.is_empty() {
            out.push_str("\nFailures (first few per file):\n");
            for f in failing {
                let variant = f.variant.map(|v| format!(" ({v})")).unwrap_or_default();
                let _ = writeln!(out, "\n`{}.json`{variant}, {} failed:\n", f.file, f.failed);
                for m in &f.failures {
                    let m: String = m.chars().take(600).collect();
                    let _ = writeln!(out, "- {}", m.replace('\n', " "));
                }
            }
        }
        out
    }
}

/// `packages/zero-conformance/vectors`, or `$ZERO_CONFORMANCE_VECTORS`.
pub fn vectors_dir() -> PathBuf {
    std::env::var_os("ZERO_CONFORMANCE_VECTORS").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/zero-conformance/vectors"),
        PathBuf::from,
    )
}

/// `packages/zero-conformance/generated`, next to the vectors.
pub fn generated_dir() -> PathBuf {
    vectors_dir().join("../generated")
}

/// Every suite this runner has.
pub fn suites() -> Vec<Suite> {
    suites::all()
}

/// Replays every suite, and lists the checked-in vector files and the
/// generated corpora there that no suite replays. `filter` keeps the files
/// whose names contain it.
pub fn run(filter: Option<&str>) -> Result<Report, String> {
    let vectors = vectors_dir();
    let generated = generated_dir();
    let checked_files = list_json(&vectors).map_err(|e| {
        format!(
            "can't read the conformance vectors at {}: {e}; set ZERO_CONFORMANCE_VECTORS",
            vectors.display()
        )
    })?;
    let suites = suites();
    let keep = |name: &str| filter.is_none_or(|f| name.contains(f));
    // Files several suites replay are read once.
    let mut by_file: BTreeMap<(Source, &str, Option<&str>), Vec<&Suite>> = BTreeMap::new();
    for s in &suites {
        by_file
            .entry((s.source, s.file, s.variant))
            .or_default()
            .push(s);
    }
    let claimed =
        |source: Source, name: &str| by_file.keys().any(|(s, f, _)| *s == source && *f == name);
    // Generated corpora are listed when they are there.
    let generated_files = list_json(&generated).unwrap_or_default();
    let mut files = Vec::new();
    for (source, dir, names) in [
        (Source::Checked, &vectors, &checked_files),
        (Source::Generated, &generated, &generated_files),
    ] {
        for name in names {
            if keep(name) && !claimed(source, name) {
                files.push(not_implemented(dir, source, name));
            }
        }
    }
    for ((source, name, variant), suites) in &by_file {
        if !keep(name) {
            continue;
        }
        let dir = if *source == Source::Checked {
            &vectors
        } else {
            &generated
        };
        let mut report = replay(dir, *source, name, suites);
        report.variant = *variant;
        files.push(report);
    }
    files.sort_by(|a, b| (a.source, &a.file, a.variant).cmp(&(b.source, &b.file, b.variant)));
    Ok(Report { files })
}

fn list_json(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut names: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".json")
                .map(str::to_owned)
        })
        .collect();
    names.sort();
    Ok(names)
}

fn read(dir: &Path, name: &str) -> Result<Option<Value>, String> {
    let path = dir.join(format!("{name}.json"));
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let file = json::parse(&text).map_err(|e| e.to_string())?;
    // Generated corpora have no envelope version of their own.
    if let Some(v) = file.get("formatVersion")
        && v.as_f64() != Some(FORMAT_VERSION)
    {
        return Err(format!(
            "format version {v}; this runner reads {FORMAT_VERSION}"
        ));
    }
    if file.get("cases").and_then(Value::as_array).is_none() {
        return Err("no cases".into());
    }
    Ok(Some(file))
}

fn empty(name: &str, source: Source, status: Status, cases: usize) -> FileReport {
    FileReport {
        file: name.to_owned(),
        source,
        variant: None,
        status,
        cases,
        passed: 0,
        exceptions: BTreeMap::new(),
        failed: 0,
        failures: Vec::new(),
    }
}

fn not_implemented(dir: &Path, source: Source, name: &str) -> FileReport {
    match read(dir, name) {
        Ok(file) => {
            let cases = file
                .as_ref()
                .and_then(|f| f.get("cases")?.as_array())
                .map_or(0, Vec::len);
            empty(name, source, Status::NotImplemented, cases)
        }
        Err(e) => empty(name, source, Status::Unreadable(e), 0),
    }
}

fn replay(dir: &Path, source: Source, name: &str, suites: &[&Suite]) -> FileReport {
    let file = match read(dir, name) {
        Ok(Some(file)) => file,
        Ok(None) => return empty(name, source, Status::Missing, 0),
        Err(e) => return empty(name, source, Status::Unreadable(e), 0),
    };
    let cases = file
        .get("cases")
        .and_then(Value::as_array)
        .expect("checked in read");
    let mut report = empty(name, source, Status::Replayed, cases.len());
    for (i, c) in cases.iter().enumerate() {
        // A case passes when every suite of its file passes it, and matches
        // under an exception when one of them needed one.
        let mut outcome = Ok(Outcome::Pass);
        for suite in suites {
            let result = catch_unwind(AssertUnwindSafe(|| (suite.check)(c, &file)))
                .unwrap_or_else(|panic| Err(format!("panicked: {}", panic_message(&*panic))));
            match result {
                Ok(Outcome::Pass) => {}
                exception @ Ok(Outcome::Exception(_)) => outcome = exception,
                failure @ Err(_) => {
                    outcome = failure;
                    break;
                }
            }
        }
        match outcome {
            Ok(Outcome::Pass) => report.passed += 1,
            Ok(Outcome::Exception(e)) => *report.exceptions.entry(e).or_default() += 1,
            Err(message) => {
                report.failed += 1;
                if report.failures.len() < KEPT_FAILURES {
                    report.failures.push(format!("case {i}: {message}"));
                }
            }
        }
    }
    report
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic".to_owned())
}
