//! zbugs' issue list after its first sync: the changes it sees, which go
//! through its joins, `exists`, and limit, and opening its pages, as a list
//! that grows its limit while it scrolls does.

use crate::first_sync;
use crate::fixtures::Client;
use crate::harness::{Bench, check};
use std::cell::Cell;
use zero_client::client::{MaterializedView, Mutator};
use zero_client::ivm::ViewValue;
use zero_client::json::{self, Object, Value};

/// The puts of the first sync, parsed: each row's table and value.
fn parse(rows: &[String]) -> Vec<(String, Object)> {
    rows.iter()
        .map(|r| {
            let op = json::parse(r).unwrap();
            let table = op.get("tableName").and_then(Value::as_str).unwrap();
            let value = op.get("value").and_then(Value::as_object).unwrap();
            (table.to_owned(), value.clone())
        })
        .collect()
}

fn put(table: &str, value: &Object) -> String {
    format!(
        r#"{{"op":"put","tableName":"{table}","value":{}}}"#,
        json::stringify(&Value::from(value.clone()))
    )
}

fn string(row: &Object, column: &str) -> String {
    row.get(column).and_then(Value::as_str).unwrap().to_owned()
}

/// `updateIssue({id, title})`.
fn mutators() -> Vec<Mutator<()>> {
    vec![Mutator::new("updateIssue", |tx, args, _| {
        let args = args.and_then(Value::as_object).cloned().unwrap_or_default();
        tx.table("issue").update(&args)?;
        Ok(None)
    })]
}

/// A client on memory storage with the first sync's rows, `scale` times
/// as many issues.
fn synced(scale: usize) -> (Client, Vec<(String, Object)>) {
    let rows = first_sync::rows_times(scale);
    let client = Client::with(0, true, first_sync::schema(), mutators());
    let per_part = rows.len().div_ceil(first_sync::PARTS);
    let parts: Vec<Vec<String>> = rows.chunks(per_part).map(<[String]>::to_vec).collect();
    client.receive(&client.next_poke(&parts, None));
    (client, parse(&rows))
}

fn ids(view: &MaterializedView) -> Vec<String> {
    view.data()
        .entries()
        .iter()
        .map(|e| string(&e.row, "id"))
        .collect()
}

fn labels(view: &MaterializedView, index: usize) -> usize {
    match view.data().entries()[index].relationship("labels") {
        Some(ViewValue::Many(labels)) => labels.len(),
        _ => 0,
    }
}

pub fn issue_list(bench: &mut Bench) {
    bench.group("issue list", |bench| {
        let (client, rows) = synced(1);
        let schema = client.zero.schema().clone();
        let view = client.materialize(&first_sync::issue_list(&schema));
        let issue = |id: &str| {
            rows.iter()
                .find(|(t, r)| t == "issue" && string(r, "id") == id)
                .map(|(_, r)| r.clone())
                .unwrap()
        };
        let page = ids(&view);
        check(page.len() == first_sync::PAGE_SIZE + 1, "a page of issues");
        let counter = Cell::new(0usize);

        // Another client retitled an issue on the page.
        let mut retitled = issue(&page[10]);
        bench.run_with(
            "poke retitling an issue on the page",
            || {
                counter.set(counter.get() + 1);
                retitled.insert("title", Value::from(format!("Title {}", counter.get())));
                client.next_poke(&[vec![put("issue", &retitled)]], None)
            },
            |frames| client.receive(frames),
        );
        if counter.get() > 0 {
            let title = format!("Title {}", counter.get());
            check(
                view.data().entries()[10].row.get("title") == Some(&Value::from(title)),
                "retitled",
            );
        }

        // Another client changed an issue below the page, which moves it to
        // the top and the page's last issue off it. The issues taken in turn
        // are the open public ones of the project, oldest change first, so
        // each is below the page when it changes.
        let mut below: Vec<Object> = rows
            .iter()
            .filter(|(t, r)| {
                t == "issue"
                    && r.get("open") == Some(&Value::from(true))
                    && r.get("visibility") == Some(&Value::from("public"))
            })
            .map(|(_, r)| r.clone())
            .collect();
        below.sort_by(|a, b| {
            let m = |r: &Object| r.get("modified").and_then(Value::as_f64).unwrap();
            m(a).total_cmp(&m(b))
        });
        check(below.len() > 2 * page.len(), "issues below the page");
        let latest = 2e12;
        let moves = Cell::new(0usize);
        let moved = Cell::new(String::new());
        bench.run_with(
            "poke moving an issue below the page to its top",
            || {
                moves.set(moves.get() + 1);
                let next = moves.get() % below.len();
                let row = &mut below[next];
                row.insert("modified", Value::from(latest + moves.get() as f64));
                moved.set(string(row, "id"));
                client.next_poke(&[vec![put("issue", row)]], None)
            },
            |frames| client.receive(frames),
        );
        let top = moved.take();
        if !top.is_empty() {
            check(ids(&view)[0] == top, "moved to the top");
        }

        // Another client labeled an issue on the page, or took the label off.
        let first = ids(&view)[0].clone();
        let project = string(&issue(&first), "projectID");
        let label = rows
            .iter()
            .filter(|(t, r)| t == "label" && string(r, "projectID") == project)
            .map(|(_, r)| string(r, "id"))
            .find(|l| {
                !rows.iter().any(|(t, r)| {
                    t == "issueLabel" && string(r, "issueID") == first && string(r, "labelID") == *l
                })
            })
            .unwrap();
        let before = labels(&view, 0);
        let mut issue_label = Object::new();
        issue_label.insert("issueID", Value::from(first.as_str()));
        issue_label.insert("labelID", Value::from(label.as_str()));
        issue_label.insert("projectID", Value::from(project.as_str()));
        let labeled = Cell::new(false);
        bench.run_with(
            "poke labeling an issue on the page, or unlabeling it",
            || {
                labeled.set(!labeled.get());
                let op = if labeled.get() {
                    put("issueLabel", &issue_label)
                } else {
                    format!(
                        r#"{{"op":"del","tableName":"issueLabel","id":{{"labelID":"{label}","issueID":"{first}"}}}}"#
                    )
                };
                client.next_poke(&[vec![op]], None)
            },
            |frames| client.receive(frames),
        );
        check(
            labels(&view, 0) == before + usize::from(labeled.get()),
            "labeled",
        );

        // This client retitles an issue on the page.
        let id = ids(&view)[10].clone();
        let before = counter.get();
        bench.run("optimistic update retitling an issue on the page", || {
            counter.set(counter.get() + 1);
            let mut args = Object::new();
            args.insert("id", Value::from(id.as_str()));
            args.insert("title", Value::from(format!("Title {}", counter.get())));
            let _ = client.zero.mutate("updateIssue", Some(Value::from(args)));
        });
        if counter.get() > before {
            let title = format!("Title {}", counter.get());
            check(
                view.data().entries()[10].row.get("title") == Some(&Value::from(title)),
                "retitled here",
            );
        }
        view.destroy();
        drop(client);

        // Opening the list, and opening it again with a larger limit, as
        // zbugs-gpui and zbugs-rune grow it while it scrolls.
        let (client, _) = synced(4);
        for limit in [first_sync::PAGE_SIZE + 1, 1000] {
            let query = first_sync::issue_list(client.zero.schema()).limit(limit as i64);
            bench.run(
                &format!("open and close the list at limit {limit}, 8097 rows"),
                || {
                    let view = client.materialize(&query);
                    check(view.data().entries().len() == limit, "the page");
                    view.destroy();
                },
            );
        }
    });
}
