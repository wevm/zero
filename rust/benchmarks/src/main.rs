//! The client's benchmarks: ports of the JS benchmarks
//! (`zero-client/src/client/zero.bench.ts`, `shared/src/hash.bench.ts`)
//! under the same names, and of the Swift client's own (memory, hydration,
//! pokes, first sync, and mutations), so the three clients compare.
//!
//! ```sh
//! cargo run --release -p zero-benchmarks -- [--time <seconds>] [--json <file>] [<filter>…]
//! ```
//!
//! A filter is a substring of `group > benchmark`, such as `hash` or
//! `pokes > put 100`.

mod first_sync;
mod fixtures;
mod harness;

use fixtures::{Client, sum, update_user_args, user_json, users};
use harness::{Bench, check, resident_memory};
use std::cell::Cell;
use std::hint::black_box;
use zero_client::ast::SimpleOperator;
use zero_client::hash::xxhash32;
use zero_client::ivm::{ViewEntry, ViewValue};
use zero_client::json::Value;
use zero_client::typed::{QueryElement, QueryValue, ZeroValueError, column};

fn main() {
    let mut filters = Vec::new();
    let mut budget = 1.0;
    let mut json = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--time" => budget = args.next().and_then(|t| t.parse().ok()).unwrap_or(budget),
            "--json" => json = args.next(),
            _ => filters.push(arg),
        }
    }
    let mut bench = Bench::new(budget, filters);

    // First, before the others allocate.
    memory(&mut bench);
    zero_bench(&mut bench);
    hydration(&mut bench);
    pokes(&mut bench);
    first_sync(&mut bench);
    mutations(&mut bench);
    hash(&mut bench);

    if let Some(path) = json {
        let text = zero_client::json::stringify(&bench.results());
        std::fs::write(&path, text + "\n").unwrap_or_else(|e| panic!("writing {path}: {e}"));
    }
}

/// What 100k rows cost in memory: the rows in the replica and the sources,
/// and a view of them all.
fn memory(bench: &mut Bench) {
    bench.group("memory", |bench| {
        let n = 100_000;
        let before = resident_memory();
        let client = Client::new(n, false);
        let view = client.materialize(&users(client.zero.schema()));
        check(view.data().entries().len() == n, "the view has every row");
        let bytes = before
            .zip(resident_memory())
            .map(|(b, a)| a.saturating_sub(b));
        bench.record_bytes(
            &format!("{n} rows x 10 columns (numbers), with a view"),
            bytes,
        );
    });
}

/// `zero.bench.ts`: views of 1000 rows of 10 number columns.
fn zero_bench(bench: &mut Bench) {
    bench.group("zero.bench.ts", |bench| {
        let n = 1000;
        let client = Client::new(n, false);
        let all = users(client.zero.schema());
        bench.run(&format!("All {n} rows x 10 columns (numbers)"), || {
            let view = client.materialize(&all);
            check(sum(&view) == ((n - 1) * n / 2) as f64, "every row");
            view.destroy();
        });
        let last = all.clone().where_("a", SimpleOperator::Eq, (n - 1) as f64);
        bench.run("pk = N", || {
            let view = client.materialize(&last);
            check(sum(&view) == (n - 1) as f64, "the last row");
            view.destroy();
        });
        let lower = all.where_("a", SimpleOperator::Lt, (n / 2) as f64);
        bench.run(
            &format!("Lower rows {} x 10 columns (numbers)", n / 2),
            || {
                let view = client.materialize(&lower);
                check(sum(&view) == ((n / 2 - 1) * n / 4) as f64, "the lower half");
                view.destroy();
            },
        );
    });
}

/// `zero.bench.ts`'s row as a struct, as `zero-rust codegen` generates it.
struct User {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
    g: f64,
    h: f64,
    i: f64,
    j: f64,
}

impl QueryElement for User {
    fn from_entry(entry: &ViewEntry) -> Result<Self, ZeroValueError> {
        Ok(User {
            a: column(entry, "a", 0)?,
            b: column(entry, "b", 1)?,
            c: column(entry, "c", 2)?,
            d: column(entry, "d", 3)?,
            e: column(entry, "e", 4)?,
            f: column(entry, "f", 5)?,
            g: column(entry, "g", 6)?,
            h: column(entry, "h", 7)?,
            i: column(entry, "i", 8)?,
            j: column(entry, "j", 9)?,
        })
    }
}

impl User {
    fn total(&self) -> f64 {
        self.a + self.b + self.c + self.d + self.e + self.f + self.g + self.h + self.i + self.j
    }
}

/// Views of many rows, as the view's entries and as row structs.
fn hydration(bench: &mut Bench) {
    bench.group("hydration", |bench| {
        for n in [1_000, 10_000, 100_000] {
            if !bench.wants(&format!("materialize {n} rows"))
                && !bench.wants(&format!("materialize {n} rows as structs"))
            {
                continue;
            }
            let client = Client::new(n, false);
            let all = users(client.zero.schema());
            bench.run(&format!("materialize {n} rows"), || {
                let view = client.materialize(&all);
                check(view.data().entries().len() == n, "every row");
                view.destroy();
            });
            bench.run(&format!("materialize {n} rows as structs"), || {
                let view = client.materialize(&all);
                let rows = Vec::<User>::from_view(&view.data()).unwrap();
                check(rows.len() == n, "every row");
                black_box(rows.iter().map(User::total).sum::<f64>());
                view.destroy();
            });
        }
    });
}

/// Column `b` of the view's `index`th row.
fn b(view: &zero_client::client::MaterializedView, index: usize) -> Option<f64> {
    let data = view.data();
    let entries = data.entries();
    entries
        .get(index)
        .and_then(|e| e.row.get("b"))
        .and_then(Value::as_f64)
}

/// Pokes from the server: parsing them, writing the replica, and updating
/// a view of 1000 rows.
fn pokes(bench: &mut Bench) {
    bench.group("pokes", |bench| {
        let n = 1000;
        let client = Client::new(n, true);
        let view = client.materialize(&users(client.zero.schema()));
        check(view.data().entries().len() == n, "the poke's rows");
        let value = Cell::new(n);
        for k in [1, 100, 1000] {
            bench.run_with(
                &format!("put {k} changed rows, {n}-row view"),
                || {
                    value.set(value.get() + 1);
                    let rows = (0..k).map(|a| user_json(a, value.get())).collect();
                    client.next_poke(&[rows], None)
                },
                |frames| client.receive(frames),
            );
            if value.get() > n {
                check(b(&view, 0) == Some(value.get() as f64), "the poke applied");
            }
        }
    });
}

/// zbugs' first sync: one poke of 2,058 rows into a fresh client with the
/// issue list's view, timed from the poke's frames arriving to the view
/// being flushed. Its rows arrive by table in alphabetical order, so the
/// issues come before the labels, the project, and the users that their
/// joins and `exists` wait for.
fn first_sync(bench: &mut Bench) {
    bench.group("first sync", |bench| {
        let rows = first_sync::rows();
        let total = rows.len();
        let per_part = total.div_ceil(first_sync::PARTS);
        let parts: Vec<Vec<String>> = rows.chunks(per_part).map(<[String]>::to_vec).collect();
        let schema = first_sync::schema();
        let client = || Client::with(0, true, schema.clone(), Vec::new());
        bench.run_with(
            &format!(
                "issueListV2 view, zbugs poke of {total} rows in {} parts",
                parts.len()
            ),
            || {
                let c = client();
                let view = c.materialize(&first_sync::issue_list(&schema));
                let frames = c.next_poke(&parts, None);
                (c, view, frames)
            },
            |(c, view, frames)| {
                c.receive(frames);
                let data = view.data();
                let entries = data.entries();
                check(
                    entries.len() == first_sync::PAGE_SIZE + 1,
                    "a page of issues",
                );
                check(
                    entries.iter().any(|e| {
                        e.relationship("creator")
                            .is_some_and(|v| !matches!(v, ViewValue::One(None)))
                    }),
                    "their creators",
                );
            },
        );
        // The same poke with no view: what the issue list adds is the
        // difference.
        bench.run_with(
            &format!(
                "no view, zbugs poke of {total} rows in {} parts",
                parts.len()
            ),
            || {
                let c = client();
                let frames = c.next_poke(&parts, None);
                (c, frames)
            },
            |(c, frames)| c.receive(frames),
        );
    });
}

/// Mutations, with a view of 1000 rows.
fn mutations(bench: &mut Bench) {
    bench.group("mutations", |bench| {
        let n = 1000;
        let value = Cell::new(n);

        // Checks the row `value` last updated, if any.
        let check_updated = |view: &zero_client::client::MaterializedView, what: &str| {
            if value.get() > n {
                check(b(view, value.get() % n) == Some(value.get() as f64), what);
            }
        };

        let local = Client::new(n, false);
        let local_view = local.materialize(&users(local.zero.schema()));
        bench.run(&format!("optimistic update, {n}-row view"), || {
            value.set(value.get() + 1);
            let args = update_user_args(value.get() % n, value.get());
            let _ = local.zero.mutate("updateUser", Some(args));
        });
        check_updated(&local_view, "updated");

        // The client's side of a round trip: the optimistic update, the
        // push, and the poke that confirms it, which rebases the pending
        // mutations.
        let remote = Client::new(n, true);
        let remote_view = remote.materialize(&users(remote.zero.schema()));
        let last_mutation_id = Cell::new(0);
        bench.run(&format!("round trip, {n}-row view"), || {
            value.set(value.get() + 1);
            last_mutation_id.set(last_mutation_id.get() + 1);
            let a = value.get() % n;
            let _ = remote
                .zero
                .mutate("updateUser", Some(update_user_args(a, value.get())));
            remote.scheduler.run_due_timers();
            remote.receive(&remote.next_poke(
                &[vec![user_json(a, value.get())]],
                Some(last_mutation_id.get()),
            ));
        });
        check_updated(&remote_view, "confirmed");
    });
}

/// `hash.bench.ts`: the hashes of row keys and query ASTs.
fn hash(bench: &mut Bench) {
    fn input(length: usize) -> String {
        let mut s = String::new();
        let mut i = 0;
        while s.len() < length {
            s += &format!(r#"{{"table":"issue{i}","column":"created{i}","value":{i}}},"#);
            i += 1;
        }
        s.truncate(length);
        s
    }
    fn group<T>(bench: &mut Bench, name: &str, f: impl Fn(&str) -> T) {
        bench.group(&format!("hash.bench.ts > {name}"), |bench| {
            for size in [64, 256, 1024, 8192] {
                let s = input(size);
                bench.run(&format!("{size} chars"), || {
                    black_box(f(black_box(&s)));
                });
            }
        });
    }
    group(bench, "h64", xxhash32::h64);
    group(bench, "h128", xxhash32::h128);
    group(bench, "h32", xxhash32::h32);
}
