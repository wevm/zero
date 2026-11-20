//! zbugs' launch: a client made on a SQLite store that holds zbugs' first
//! sync, showing the issue list's first page, as the app opens it. Timed
//! on the app's thread: `Zero::new`, or `Zero::from_prepared` after
//! `Zero::prepare` read the store on its own thread, then materializing
//! the issue list.

use crate::first_sync;
use crate::fixtures::Client;
use crate::harness::{Bench, check};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};
use zero_client::client::{
    LogLevel, MaterializedView, PreparedZero, Zero, ZeroOptions, ZeroStorage,
};
use zero_client::testing::{ManualScheduler, RecordingLogSink};
use zero_client::ttl::Ttl;

const STORAGE_KEY: &str = "cold-start";

/// The options of the app's client, on the store in `directory`.
fn options(directory: &Path) -> ZeroOptions {
    ZeroOptions {
        cache_url: Some("http://zero.bench".into()),
        user_id: Some("bench".into()),
        storage_key: Some(STORAGE_KEY.into()),
        storage: ZeroStorage::Sqlite {
            directory: Some(directory.to_owned()),
        },
        log_level: LogLevel::Error,
        log_sink: Some(Rc::new(RecordingLogSink::default())),
        ..ZeroOptions::default()
    }
}

/// A launch: a copy of the seeded store, and the client made on it, which
/// closes before the copy is deleted.
struct Launch {
    directory: PathBuf,
    prepared: Option<PreparedZero>,
    zero: Option<Zero>,
    view: Option<MaterializedView>,
}

impl Launch {
    fn new(seed: &Path, directory: PathBuf) -> Self {
        std::fs::create_dir_all(&directory).unwrap();
        for file in std::fs::read_dir(seed).unwrap() {
            let file = file.unwrap().path();
            std::fs::copy(&file, directory.join(file.file_name().unwrap())).unwrap();
        }
        Launch {
            directory,
            prepared: None,
            zero: None,
            view: None,
        }
    }

    /// Materializes the issue list on `zero`, as the first frame shows it.
    fn show(&mut self, zero: Zero) {
        let view = zero
            .materialize(&first_sync::issue_list(zero.schema()), &Ttl::default())
            .unwrap();
        check(
            view.data().entries().len() == first_sync::PAGE_SIZE + 1,
            "the stored issues",
        );
        self.view = Some(view);
        self.zero = Some(zero);
    }
}

impl Drop for Launch {
    fn drop(&mut self) {
        self.view.take();
        self.prepared.take();
        if let Some(zero) = self.zero.take() {
            zero.close();
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

pub fn cold_start(bench: &mut Bench) {
    bench.group("cold start", |bench| {
        let root = std::env::temp_dir().join(format!("zero-bench-cold-{}", std::process::id()));
        for scale in [1, 4] {
            let rows = first_sync::rows_times(scale);
            let total = rows.len();
            let names = [
                format!("Zero::new, issue list, {total} rows"),
                format!("from_prepared, issue list, {total} rows"),
                format!("prepare then from_prepared at once, issue list, {total} rows"),
            ];
            if !names.iter().any(|n| bench.wants(n)) {
                continue;
            }
            let seed = root.join(format!("seed-{scale}"));
            let schema = first_sync::schema();
            {
                let storage = options(&seed).storage;
                let client = Client::on(
                    (storage, STORAGE_KEY.to_owned()),
                    0,
                    true,
                    schema.clone(),
                    Vec::new(),
                );
                let per_part = total.div_ceil(first_sync::PARTS);
                let parts: Vec<Vec<String>> =
                    rows.chunks(per_part).map(<[String]>::to_vec).collect();
                client.receive(&client.next_poke(&parts, None));
                client.zero.flush_storage();
                client.keep_store();
            }
            let mut runs = 0;
            let mut launch = || {
                runs += 1;
                Launch::new(&seed, root.join(format!("run-{runs}")))
            };
            let scheduler = ManualScheduler::new(1_700_000_000_000.0);
            let open = |l: &mut Launch| {
                let zero = Zero::new(
                    schema.clone(),
                    (),
                    Vec::new(),
                    options(&l.directory),
                    scheduler.clone(),
                    None,
                )
                .unwrap();
                l.show(zero);
            };
            // How long the store takes to read here, so that `prepare` has
            // read it before the client takes over.
            let read = {
                let mut l = launch();
                let start = Instant::now();
                open(&mut l);
                start.elapsed()
            };
            bench.run_with(&names[0], &mut launch, open);
            bench.run_with(
                &names[1],
                || {
                    let mut l = launch();
                    let options = options(&l.directory);
                    l.prepared = Some(Zero::prepare(schema.clone(), (), Vec::new(), options));
                    std::thread::sleep(read * 3 + Duration::from_millis(5));
                    l
                },
                |l| {
                    let prepared = l.prepared.take().unwrap();
                    let zero = Zero::from_prepared(prepared, scheduler.clone(), None).unwrap();
                    l.show(zero);
                },
            );
            bench.run_with(&names[2], &mut launch, |l| {
                let prepared = Zero::prepare(schema.clone(), (), Vec::new(), options(&l.directory));
                let zero = Zero::from_prepared(prepared, scheduler.clone(), None).unwrap();
                l.show(zero);
            });
        }
        let _ = std::fs::remove_dir_all(&root);
    });
}
