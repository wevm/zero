//! `cargo run -p zero-conformance -- [--require-generated] [--summary PATH] [FILTER]`
//!
//! Replays the conformance vectors and prints a Markdown report. Exits 1 when
//! a replayed case differs from JS, and with `--require-generated` when the
//! seeded fuzz corpora are missing. `--summary` also appends the report to a
//! file, such as `$GITHUB_STEP_SUMMARY`.

use std::io::Write as _;
use std::process::ExitCode;
use zero_conformance::{Source, run};

fn main() -> ExitCode {
    let mut require_generated = false;
    let mut summary = None;
    let mut filter = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--require-generated" => require_generated = true,
            "--summary" => summary = args.next(),
            _ if !arg.starts_with('-') => filter = Some(arg),
            _ => {
                eprintln!(
                    "usage: zero-conformance [--require-generated] [--summary PATH] [FILTER]"
                );
                return ExitCode::from(2);
            }
        }
    }
    // A panicking case is reported as a failure; don't print each one.
    std::panic::set_hook(Box::new(|_| {}));
    let report = match run(filter.as_deref()) {
        Ok(report) => report,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let mut text = format!("## Rust conformance\n\n{}", report.markdown());
    let missing = report.missing(Source::Generated);
    if !missing.is_empty() {
        text.push_str(&format!(
            "\nGenerated corpora missing (run `pnpm --filter zero-conformance run test`): {}\n",
            missing.join(", ")
        ));
    }
    print!("{text}");
    if let Some(path) = summary {
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| f.write_all(text.as_bytes()));
        if let Err(e) = written {
            eprintln!("can't write {path}: {e}");
            return ExitCode::FAILURE;
        }
    }
    if !report.ok() || (require_generated && !missing.is_empty()) {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
