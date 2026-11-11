//! Client scenarios: `client/client.test.ts`, `client/harden-*.test.ts`,
//! `harden-queries`, and `client/bughunt.test.ts` with its seeded
//! aggregate scenarios (`generated/client-aggregates-fuzz`).
//!
//! Each case is a scenario of steps an app and a server take
//! (`client/harness.ts`): materializing views, mutating, receiving server
//! messages on a mock socket, moving a fake clock. The runner replays it on
//! a `Zero` with a manual clock and mock sockets, on memory storage, and
//! compares what the app and the server observe after every step with what
//! the JS client did. It mirrors `harness.ts`; the queries and mutators are
//! those of `client/app.ts`.

use super::ast::test_schema;
use crate::case::Case;
use crate::{Outcome, Source, Suite};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use zero_client::ast::{Direction, JsonPathSegment, SimpleOperator, System};
use zero_client::client::{
    ApplicationError, Error, MaterializedView, Mutator, RunType, SocketMessage, Transaction, Zero,
    ZeroOptions, ZeroStorage,
};
use zero_client::decode::JsonCodable;
use zero_client::json::{self, Object, Value};
use zero_client::protocol::{poke_chunk, sec_protocols};
use zero_client::query::{AnyQuery, ExistsOptions};
use zero_client::schema::Schema;
use zero_client::testing::{ManualScheduler, MockSocket, MockSocketFactory, RecordingLogSink};
use zero_client::ttl::Ttl;

pub fn suites() -> Vec<Suite> {
    [
        "client",
        "client-harden-connection",
        "client-harden-mutations",
        "harden-queries",
        "bughunt-client",
    ]
    .into_iter()
    .map(|file| Suite {
        variant: None,
        file,
        source: Source::Checked,
        check: scenario,
    })
    // The seeded aggregate scenarios of `client/bughunt.test.ts`.
    .chain([Suite {
        variant: None,
        file: "client-aggregates-fuzz",
        source: Source::Generated,
        check: scenario,
    }])
    .flat_map(|s| {
        // Each again on SQLite storage, in a directory of its own, so a
        // relaunch reads what the last client wrote to disk.
        [
            s,
            Suite {
                variant: Some("sqlite"),
                check: scenario_on_sqlite,
                ..s
            },
        ]
    })
    .collect()
}

thread_local! {
    /// Where the scenario running on this thread keeps its replica.
    static STORAGE: RefCell<ZeroStorage> = RefCell::default();
}

fn scenario(case: &Value, file: &Value) -> Result<Outcome, String> {
    STORAGE.with(|s| *s.borrow_mut() = ZeroStorage::Memory);
    run_scenario(case, file)
}

fn scenario_on_sqlite(case: &Value, file: &Value) -> Result<Outcome, String> {
    static DIRECTORIES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = DIRECTORIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let directory =
        std::env::temp_dir().join(format!("zero-conformance-{}-{n}", std::process::id()));
    STORAGE.with(|s| {
        *s.borrow_mut() = ZeroStorage::Sqlite {
            directory: Some(directory.clone()),
        }
    });
    let result = run_scenario(case, file);
    STORAGE.with(|s| *s.borrow_mut() = ZeroStorage::Memory);
    let _ = std::fs::remove_dir_all(&directory);
    result
}

fn run_scenario(case: &Value, file: &Value) -> Result<Outcome, String> {
    let start_time = file
        .get("shared")
        .and_then(|s| s.get("startTime"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let schema = test_schema()?.clone();
    let runner = Runner::new(case.clone(), start_time, schema)?;
    let actual = runner.run();
    let expected = case.arr("expect")?;
    match first_mismatch(case, expected, &actual) {
        Some(m) => Err(m),
        None => Ok(Outcome::Pass),
    }
}

fn first_mismatch(scenario: &Value, expected: &[Value], actual: &[Value]) -> Option<String> {
    let steps = scenario
        .get("steps")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for i in 0..expected.len().max(actual.len()) {
        let e = expected.get(i).unwrap_or(&Value::Null);
        let a = actual.get(i).unwrap_or(&Value::Null);
        if e == a {
            continue;
        }
        let step = steps.get(i).map_or("?".to_owned(), json::stringify);
        let mut lines = vec![format!("step {i}: {step}")];
        let empty = Object::new();
        let (eo, ao) = (
            e.as_object().unwrap_or(&empty),
            a.as_object().unwrap_or(&empty),
        );
        let mut keys: Vec<&String> = eo.keys().chain(ao.keys()).collect();
        keys.sort();
        keys.dedup();
        for key in keys {
            if eo.get(key) != ao.get(key) {
                let show = |v: Option<&Value>| v.map_or("(none)".to_owned(), json::stringify);
                lines.push(format!("  {key}:"));
                lines.push(format!("    expected {}", show(eo.get(key))));
                lines.push(format!("    actual   {}", show(ao.get(key))));
            }
        }
        return Some(lines.join("\n"));
    }
    None
}

// MARK: The app (`client/app.ts`)

fn zql(table: &str) -> AnyQuery {
    let schema = test_schema().expect("test schema").clone();
    AnyQuery::new(schema, table, System::Client)
}

fn s(v: &str) -> Value {
    Value::from(v)
}

fn cols(c: &[&str]) -> Vec<String> {
    c.iter().map(|c| (*c).to_owned()).collect()
}

fn json_path(key: &str) -> Vec<JsonPathSegment> {
    vec![JsonPathSegment::Key(key.to_owned())]
}

use Direction::{Asc, Desc};
use SimpleOperator::{Eq, Gt, Ne};

/// The named query `name` for `args` (`None` for undefined).
fn query(name: &str, args: Option<&Value>) -> Option<AnyQuery> {
    let arg = || args.cloned().unwrap_or(Value::Null);
    let q = match name {
        "issues.all" => zql("issue").order_by("id", Asc),
        "issues.open" => zql("issue")
            .where_("closed", Eq, false)
            .related("comments", |q| q.order_by("id", Asc))
            .order_by("createdAt", Asc),
        "issues.byOwner" => zql("issue")
            .where_("ownerId", Eq, arg())
            .order_by("id", Asc),
        "issues.byID" => zql("issue")
            .where_("id", Eq, arg())
            .related("owner", |q| q)
            .one(),
        "issues.firstTwo" => zql("issue").order_by("title", Asc).limit(2),
        "issues.withAuthoredComments" => zql("issue")
            .where_exists("comments", ExistsOptions::default(), |q| {
                q.where_exists(
                    "author",
                    ExistsOptions {
                        flip: Some(true),
                        scalar: None,
                    },
                    |a| a.limit(3),
                )
            })
            .order_by("id", Asc),
        "issues.withOneComment" => zql("issue")
            .related("oneComment", |q| q)
            .order_by("id", Asc),
        "issues.withFirstComment" => zql("issue")
            .where_exists(
                "comments",
                ExistsOptions {
                    flip: Some(true),
                    scalar: None,
                },
                |q| q.order_by("id", Asc).limit(1),
            )
            .order_by("id", Asc),
        "users.all" => zql("user").order_by("id", Asc),
        "aggregates.issueCount" => zql("issue").count(),
        "aggregates.openIssueCount" => zql("issue").where_("closed", Eq, false).count(),
        "aggregates.commentCounts" => zql("issue")
            .related("comments", |q| q.count())
            .order_by("id", Asc),
        "aggregates.commentTimes" => zql("issue")
            .related("comments", |q| {
                q.where_("authorId", Ne, s("u9")).avg("createdAt")
            })
            .order_by("id", Asc),
        "aggregates.labelCounts" => zql("issue")
            .related("labels", |q| q.count())
            .order_by("id", Asc),
        "aggregates.owners" => zql("issue").distinct(cols(&["ownerId"])),
        "aggregates.commentsByIssue" => zql("comment").group_by(cols(&["issueId"])).count(),
        "aggregates.timeByAuthor" => zql("comment")
            .group_by(cols(&["authorId"]))
            .sum("createdAt"),
        "aggregates.issueWithCommentCount" => zql("issue")
            .where_("id", Eq, arg())
            .related("comments", |q| q.count())
            .one(),
        "aggregates.openByState" => zql("issue")
            .where_("closed", Eq, false)
            .group_by(cols(&["closed"]))
            .count(),
        "aggregates.issueStates" => zql("issue").distinct(cols(&["closed"])),
        "aggregates.issueStateCount" => zql("issue").distinct(cols(&["closed"])).count(),
        "aggregates.countByState" => zql("issue").group_by(cols(&["closed"])).count(),
        "aggregates.timeByState" => zql("issue").group_by(cols(&["closed"])).sum("createdAt"),
        "aggregates.avgTimeByState" => zql("issue").group_by(cols(&["closed"])).avg("createdAt"),
        "aggregates.issuesByOwner" => zql("issue")
            .group_by(cols(&["closed", "ownerId"]))
            .count()
            .order_by("ownerId", Desc),
        "aggregates.latestIssueTime" => zql("issue").max("createdAt"),
        "aggregates.commentAndLabelCounts" => zql("issue")
            .related("comments", |q| q.count())
            .related("labels", |q| q.count())
            .order_by("id", Asc),
        "aggregates.commentTimeSums" => zql("issue")
            .related("comments", |q| q.sum("createdAt"))
            .order_by("id", Asc),
        "aggregates.userIssueCounts" => zql("user")
            .related("issues", |q| q.count())
            .order_by("id", Asc),
        "aggregates.pointsByName" => zql("user")
            .group_by(cols(&["name"]))
            .sum_json("metadata", json_path("points")),
        "aggregates.avgPointsByName" => zql("user")
            .group_by(cols(&["name"]))
            .avg_json("metadata", json_path("points")),
        "aggregates.labelMax" => zql("issue")
            .related("labels", |q| q.max("name"))
            .order_by("id", Asc),
        "aggregates.bugLabelCounts" => zql("issue")
            .related("labels", |q| q.where_("name", Eq, s("bug")).count())
            .order_by("id", Asc),
        "bughunt.allComments" => zql("comment").order_by("id", Asc),
        "bughunt.allLabels" => zql("label").order_by("id", Asc),
        "bughunt.maxCommentText" => zql("comment").max("text"),
        "bughunt.minIssueTitle" => zql("issue").min("title"),
        "bughunt.pointsSum" => zql("user").sum_json("metadata", json_path("points")),
        "bughunt.ownerIssueCounts" => zql("issue")
            .related("owner", |o| o.related("issues", |i| i.count()))
            .order_by("id", Asc),
        "bughunt.commentAvgWhere" => zql("issue")
            .related("comments", |q| {
                q.where_("createdAt", Gt, 3.0).avg("createdAt")
            })
            .order_by("id", Asc),
        "bughunt.commentsByAuthorAndIssue" => zql("comment")
            .group_by(cols(&["authorId", "issueId"]))
            .count(),
        "bughunt.avgTimeByIssueWhere" => zql("comment")
            .where_("authorId", Ne, s("u3"))
            .group_by(cols(&["issueId"]))
            .avg("createdAt"),
        "bughunt.commentCountsLimited" => zql("issue")
            .related("comments", |q| q.count())
            .order_by("createdAt", Desc)
            .limit(3),
        "bughunt.ownerCount" => zql("issue").distinct(cols(&["ownerId"])).count(),
        "bughunt.sumTimeByOwner" => zql("issue").group_by(cols(&["ownerId"])).sum("createdAt"),
        "issueLabels.all" => zql("issueLabel")
            .order_by("issueId", Asc)
            .order_by("labelId", Asc),
        _ => return None,
    };
    Some(q.name_and_args(name, args.cloned().into_iter().collect()))
}

fn request(name: &str, args: Option<&Value>) -> Result<AnyQuery, String> {
    query(name, args).ok_or_else(|| format!("Unknown name {name}"))
}

/// `args[key] ?? fallback`: JS `??` treats null like a missing key.
fn field(args: Option<&Value>, key: &str, fallback: Value) -> Value {
    match args.and_then(|a| a.get(key)) {
        Some(v) if !v.is_null() => v.clone(),
        _ => fallback,
    }
}

fn issue_row(args: Option<&Value>) -> Object {
    let mut row = Object::new();
    row.insert("title", field(args, "title", s("")));
    row.insert("description", field(args, "description", s("")));
    row.insert("closed", field(args, "closed", Value::from(false)));
    row.insert("ownerId", field(args, "ownerId", Value::Null));
    row.insert("createdAt", field(args, "createdAt", Value::from(1.0)));
    if let Some(id) = args.and_then(|a| a.get("id")) {
        row.insert("id", id.clone());
    }
    row
}

fn object(v: Option<&Value>) -> Object {
    v.and_then(Value::as_object).cloned().unwrap_or_default()
}

/// Runs the CRUD operations `ops` through `tx`.
fn run_ops(tx: &Transaction, ops: Option<&Value>) -> Result<(), Error> {
    for o in ops.and_then(Value::as_array).into_iter().flatten() {
        let table = tx.table(o.get("table").and_then(Value::as_str).unwrap_or(""));
        let value = object(o.get("value"));
        match o.get("op").and_then(Value::as_str) {
            Some("insert") => table.insert(&value)?,
            Some("upsert") => table.upsert(&value)?,
            Some("update") => table.update(&value)?,
            Some("delete") => table.delete(&value)?,
            _ => return Err(Error::Message("Unknown op".into())),
        }
    }
    Ok(())
}

/// What each named query in `refs` reads through `tx`.
fn read_all(tx: &Transaction, refs: Option<&Value>) -> Result<Vec<Value>, Error> {
    refs.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|r| {
            let name = r.get("name").and_then(Value::as_str).unwrap_or("");
            let q = request(name, r.get("args")).map_err(Error::Message)?;
            Ok(tx.run(&q)?.to_json().unwrap_or(Value::Null))
        })
        .collect()
}

type Body = fn(&Transaction, Option<&Value>) -> Result<Option<Value>, Error>;

fn mutators() -> Vec<(&'static str, Body)> {
    vec![
        ("issue.create", |tx, args| {
            tx.table("issue").insert(&issue_row(args))?;
            Ok(None)
        }),
        ("issue.upsert", |tx, args| {
            tx.table("issue").upsert(&issue_row(args))?;
            Ok(None)
        }),
        ("issue.update", |tx, args| {
            tx.table("issue").update(&object(args))?;
            Ok(None)
        }),
        ("issue.delete", |tx, args| {
            let mut id = Object::new();
            id.insert("id", args.cloned().unwrap_or(Value::Null));
            tx.table("issue").delete(&id)?;
            Ok(None)
        }),
        ("issue.close", |tx, args| {
            let mut row = Object::new();
            row.insert("id", args.cloned().unwrap_or(Value::Null));
            row.insert("closed", Value::from(true));
            tx.table("issue").update(&row)?;
            Ok(None)
        }),
        // Reads through the transaction, so a rebase can change what it
        // writes.
        ("issue.closeAllOpen", |tx, _| {
            let open = tx.run(&zql("issue").where_("closed", Eq, false))?;
            for issue in open.entries() {
                let mut row = Object::new();
                row.insert("id", issue.row.get("id").cloned().unwrap_or(Value::Null));
                row.insert("closed", Value::from(true));
                tx.table("issue").update(&row)?;
            }
            Ok(None)
        }),
        // Fails on a rebase that finds the issue already there.
        ("issue.createUnique", |tx, args| {
            let id = args
                .and_then(|a| a.get("id"))
                .cloned()
                .unwrap_or(Value::Null);
            let existing = tx.run(&zql("issue").where_("id", Eq, id).one())?;
            if !existing.entries().is_empty() {
                let mut details = Object::new();
                details.insert("id", field(args, "id", Value::Null));
                return Err(ApplicationError::with_details(
                    "issue exists",
                    Some(Value::Object(details)),
                )
                .into());
            }
            tx.table("issue").insert(&issue_row(args))?;
            Ok(None)
        }),
        // Returns data to the caller of the optimistic run.
        ("issue.createReturning", |tx, args| {
            tx.table("issue").insert(&issue_row(args))?;
            let mut o = Object::new();
            o.insert("created", field(args, "id", Value::Null));
            Ok(Some(Value::Object(o)))
        }),
        ("issue.fail", |_, args| {
            Err(Error::Message(
                args.and_then(Value::as_str)
                    .unwrap_or("undefined")
                    .to_owned(),
            ))
        }),
        // Writes and then fails, so nothing it wrote may stay.
        ("issue.createThenFail", |tx, args| {
            tx.table("issue").insert(&issue_row(args))?;
            Err(ApplicationError::new("failed after writing").into())
        }),
        // Runs CRUD operations on any table, then fails, reads tables back,
        // or returns `return` (`harden.ops` in app.ts).
        ("harden.ops", |tx, args| {
            let get = |k: &str| args.and_then(|a| a.get(k));
            if get("failOnRebase") == Some(&Value::from(true))
                && tx.reason() == zero_client::client::Reason::Rebase
            {
                return Err(ApplicationError::new("fails on rebase").into());
            }
            run_ops(tx, get("ops"))?;
            if let Some(fail) = get("fail").and_then(Value::as_str) {
                return Err(ApplicationError::with_details(fail, get("details").cloned()).into());
            }
            if let Some(read) = get("read").and_then(Value::as_array) {
                let mut out = Object::new();
                for t in read.iter().filter_map(Value::as_str) {
                    out.insert(t, tx.run(&zql(t))?.to_json().unwrap_or(Value::Null));
                }
                let mut o = Object::new();
                o.insert("mutationID", Value::from(tx.mutation_id()));
                o.insert("read", Value::Object(out));
                return Ok(Some(Value::Object(o)));
            }
            Ok(get("return").cloned())
        }),
        // Runs CRUD operations, then each named query through the
        // transaction, and returns what they read (`aggregates.read`).
        ("aggregates.read", |tx, args| {
            let get = |k: &str| args.and_then(|a| a.get(k));
            run_ops(tx, get("ops"))?;
            Ok(Some(Value::Array(read_all(tx, get("queries"))?)))
        }),
        // Reads the queries, runs the operations, reads them again, and
        // returns both reads; with `record`, also upserts that user with
        // the reason of the run and the reads (`aggregates.record`).
        ("aggregates.record", |tx, args| {
            let get = |k: &str| args.and_then(|a| a.get(k));
            let before = read_all(tx, get("queries"))?;
            run_ops(tx, get("ops"))?;
            let after = read_all(tx, get("queries"))?;
            let mut reads = Object::new();
            reads.insert("before", Value::Array(before));
            reads.insert("after", Value::Array(after));
            let reads = Value::Object(reads);
            if let Some(id) = get("record").and_then(Value::as_str) {
                let mut row = Object::new();
                row.insert("id", s(id));
                row.insert("name", s(tx.reason().as_str()));
                row.insert("metadata", reads.clone());
                tx.table("user").upsert(&row)?;
            }
            Ok(Some(reads))
        }),
        ("comment.add", |tx, args| {
            let mut row = Object::new();
            row.insert("authorId", field(args, "authorId", s("u1")));
            row.insert("text", field(args, "text", s("")));
            row.insert("createdAt", field(args, "createdAt", Value::from(1.0)));
            if let Some(id) = args.and_then(|a| a.get("id")) {
                row.insert("id", id.clone());
            }
            if let Some(issue_id) = args.and_then(|a| a.get("issueId")) {
                row.insert("issueId", issue_id.clone());
            }
            tx.table("comment").insert(&row)?;
            Ok(None)
        }),
    ]
}

// MARK: The runner (`runScenario` in `harness.ts`)

struct ViewEntry {
    view: MaterializedView,
    data: RefCell<Value>,
    result_type: RefCell<String>,
    notifications: Cell<u32>,
}

type Settled = Rc<RefCell<Value>>;

fn pending() -> Settled {
    Rc::new(RefCell::new(s("pending")))
}

/// A listener added by a `listen` step.
#[derive(Default)]
struct ListenerState {
    remove: RefCell<Option<Box<dyn Fn()>>>,
    calls: RefCell<Vec<Value>>,
    threw: RefCell<Option<String>>,
}

/// A preload's name, how its `cached` and `complete` settled, and its
/// cleanup.
type PreloadEntry = (String, Settled, Settled, Rc<dyn Fn()>);
type Unsubscribe = Box<dyn Fn()>;

struct Runner {
    me: Weak<Runner>,
    scenario: Value,
    schema: Arc<Schema>,
    scheduler: Rc<ManualScheduler>,
    factory: Rc<MockSocketFactory>,
    zero: RefCell<Rc<Zero>>,
    events: Rc<RefCell<Vec<Value>>>,
    placeholders: RefCell<HashMap<String, String>>,
    /// The IDs placeholders in downstream messages stand for.
    ids: RefCell<HashMap<String, String>>,
    views: RefCell<Vec<(String, Rc<ViewEntry>)>>,
    retired: RefCell<Vec<Rc<ViewEntry>>>,
    mutations: RefCell<Vec<(String, Settled, Settled)>>,
    runs: RefCell<Vec<(String, Settled)>>,
    preloads: RefCell<Vec<PreloadEntry>>,
    listeners: RefCell<Vec<(String, Rc<ListenerState>)>>,
    /// The scenario's own storage, as `#scenario-<n>` in `harness.ts`.
    scenario_key: u64,
    restarts: Cell<u32>,
}

thread_local! {
    static SCENARIO_COUNT: Cell<u64> = const { Cell::new(0) };
}

fn ttl(v: Option<&Value>) -> Option<Ttl> {
    match v? {
        Value::Number(n) => Some(Ttl::Milliseconds(*n)),
        Value::String(s) => Some(Ttl::String(s.clone())),
        _ => None,
    }
}

fn string_record(v: Option<&Value>) -> Option<Vec<(String, String)>> {
    v.and_then(Value::as_object).map(|o| {
        o.iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
            .collect()
    })
}

fn settled<T>(r: &Result<T, Error>) -> Value {
    match r {
        Ok(_) => s("resolved"),
        Err(Error::Query(e)) => {
            let mut o = Object::new();
            o.insert(
                "rejected",
                e.error.as_ref().map_or(Value::Null, JsonCodable::to_json),
            );
            Value::Object(o)
        }
        Err(_) => {
            let mut o = Object::new();
            o.insert("rejected", Value::Object(Object::new()));
            Value::Object(o)
        }
    }
}

impl Runner {
    fn new(scenario: Value, start_time: f64, schema: Arc<Schema>) -> Result<Rc<Runner>, String> {
        let scenario_key = SCENARIO_COUNT.with(|c| {
            c.set(c.get() + 1);
            c.get()
        });
        let scheduler = ManualScheduler::new(start_time);
        let factory = MockSocketFactory::new();
        let events: Rc<RefCell<Vec<Value>>> = Rc::default();
        let options = scenario.get("options").cloned().unwrap_or_default();
        let zero = Self::make_zero(
            &schema,
            &options,
            None,
            &scheduler,
            &factory,
            &events,
            scenario_key,
        )?;
        let placeholders = HashMap::from([
            (zero.client_id().to_owned(), "$clientID".to_owned()),
            (
                zero.client_group_id().to_owned(),
                "$clientGroupID".to_owned(),
            ),
        ]);
        let ids = HashMap::from([("$clientID".to_owned(), zero.client_id().to_owned())]);
        Ok(Rc::new_cyclic(|me| Runner {
            me: me.clone(),
            scenario,
            schema,
            scheduler,
            factory,
            zero: RefCell::new(Rc::new(zero)),
            events,
            placeholders: RefCell::new(placeholders),
            ids: RefCell::new(ids),
            views: RefCell::default(),
            retired: RefCell::default(),
            mutations: RefCell::default(),
            runs: RefCell::default(),
            preloads: RefCell::default(),
            listeners: RefCell::default(),
            scenario_key,
            restarts: Cell::new(0),
        }))
    }

    fn make_zero(
        schema: &Arc<Schema>,
        o: &Value,
        registered: Option<&Value>,
        scheduler: &Rc<ManualScheduler>,
        factory: &Rc<MockSocketFactory>,
        events: &Rc<RefCell<Vec<Value>>>,
        scenario_key: u64,
    ) -> Result<Zero, String> {
        let get = |k: &str| o.get(k);
        let mut options = ZeroOptions {
            log_sink: Some(Rc::new(RecordingLogSink::default())),
            storage: STORAGE.with(|s| s.borrow().clone()),
            ..ZeroOptions::default()
        };
        options.cache_url = match get("cacheURL") {
            Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            _ => Some("http://zero.test".into()),
        };
        options.user_id = match get("userID") {
            Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            _ => Some("u1".into()),
        };
        options.auth = get("auth").and_then(Value::as_str).map(str::to_owned);
        if let Some(v) = get("pingTimeoutMs").and_then(Value::as_f64) {
            options.ping_timeout_ms = v;
        }
        if let Some(v) = get("disconnectTimeoutMs").and_then(Value::as_f64) {
            options.disconnect_timeout_ms = v;
        }
        if let Some(v) = get("queryChangeThrottleMs").and_then(Value::as_f64) {
            options.query_change_throttle_ms = v;
        }
        if let Some(v) = get("maxHeaderLength").and_then(Value::as_f64) {
            options.max_header_length = v as usize;
        }
        options.hidden_disconnect_delay_ms = get("hiddenTabDisconnectDelay")
            .and_then(Value::as_f64)
            .unwrap_or(300_000.0);
        options.mutate_url = get("mutateURL").and_then(Value::as_str).map(str::to_owned);
        options.mutate_headers = string_record(get("mutateHeaders"));
        options.query_url = get("queryURL").and_then(Value::as_str).map(str::to_owned);
        options.query_headers = string_record(get("queryHeaders"));
        options.storage_key = Some(format!(
            "{}#scenario-{scenario_key}",
            get("storageKey").and_then(Value::as_str).unwrap_or("")
        ));
        {
            let e = events.clone();
            options.on_update_needed = Some(Rc::new(move |reason| {
                let mut o = Object::new();
                o.insert("onUpdateNeeded", reason.to_json());
                e.borrow_mut().push(Value::Object(o));
            }));
            let events = events.clone();
            options.on_client_state_not_found = Some(Rc::new(move || {
                events.borrow_mut().push(s("onClientStateNotFound"));
            }));
        }
        let only_issues = registered.and_then(Value::as_str) == Some("issues");
        let mutators = mutators()
            .into_iter()
            .filter(|(name, _)| !only_issues || name.starts_with("issue."))
            .map(|(name, body)| Mutator::new(name, move |tx, args, _: &()| body(tx, args)))
            .collect();
        Zero::new(
            schema.clone(),
            (),
            mutators,
            options,
            scheduler.clone(),
            Some(factory.clone()),
        )
        .map_err(|e| e.to_string())
    }

    fn zero(&self) -> Rc<Zero> {
        self.zero.borrow().clone()
    }

    /// Closes the client and starts a new one on the same storage
    /// (`restart` in `harness.ts`).
    fn restart(&self, step: &Value) -> Result<(), String> {
        let old = self.zero();
        old.close();
        self.settle();
        if let Some(ms) = step.get("advance").and_then(Value::as_f64) {
            self.scheduler.advance(ms);
        }
        let n = self.restarts.get();
        self.restarts.set(n + 1);
        let options = step
            .get("options")
            .or_else(|| self.scenario.get("options"))
            .cloned()
            .unwrap_or_default();
        let new = Self::make_zero(
            &self.schema,
            &options,
            step.get("mutators"),
            &self.scheduler,
            &self.factory,
            &self.events,
            self.scenario_key,
        )?;
        {
            let mut p = self.placeholders.borrow_mut();
            let mut ids = self.ids.borrow_mut();
            p.insert(old.client_id().to_owned(), format!("$clientID.{n}"));
            ids.insert(format!("$clientID.{n}"), old.client_id().to_owned());
            if old.client_group_id() != new.client_group_id() {
                p.insert(
                    old.client_group_id().to_owned(),
                    format!("$clientGroupID.{n}"),
                );
            }
            p.insert(new.client_id().to_owned(), "$clientID".into());
            p.insert(new.client_group_id().to_owned(), "$clientGroupID".into());
            ids.insert("$clientID".into(), new.client_id().to_owned());
        }
        *self.zero.borrow_mut() = Rc::new(new);
        self.views.borrow_mut().clear();
        self.listeners.borrow_mut().clear();
        self.retired.borrow_mut().clear();
        self.mutations.borrow_mut().clear();
        self.runs.borrow_mut().clear();
        self.preloads.borrow_mut().clear();
        Ok(())
    }

    fn normalize(&self, v: &Value, key: Option<&str>) -> Value {
        let p = self.placeholders.borrow();
        normalize(&p, v, key)
    }

    fn substitute(&self, v: &Value) -> Value {
        let ids = self.ids.borrow();
        substitute(&ids, v)
    }

    fn socket(&self, index: Option<&Value>) -> Result<Rc<MockSocket>, String> {
        let sockets = self.factory.sockets();
        match index.and_then(Value::as_f64) {
            Some(i) => sockets.get(i as usize).cloned(),
            None => sockets.last().cloned(),
        }
        .ok_or_else(|| "No socket".to_owned())
    }

    fn perform(&self, step: &Value) -> Result<(), String> {
        let op = step.get("op").and_then(Value::as_str).unwrap_or("");
        let string = |k: &str| step.str(k).map(str::to_owned);
        match op {
            "materialize" => {
                let q = request(step.str("query")?, step.get("args"))?;
                let view = self
                    .zero()
                    .materialize(
                        &q,
                        &ttl(step.get("ttl")).unwrap_or(Ttl::String(Ttl::DEFAULT.into())),
                    )
                    .map_err(|e| e.to_string())?;
                let entry = Rc::new(ViewEntry {
                    view: view.clone(),
                    data: RefCell::new(Value::Null),
                    result_type: RefCell::new(String::new()),
                    notifications: Cell::new(0),
                });
                let name = string("view")?;
                // A view replaced under its name stays alive, as in JS.
                {
                    let mut views = self.views.borrow_mut();
                    let mut retired = self.retired.borrow_mut();
                    retired.extend(
                        views
                            .iter()
                            .filter(|(n, _)| *n == name)
                            .map(|(_, v)| v.clone()),
                    );
                    views.retain(|(n, _)| *n != name);
                    views.push((name, entry.clone()));
                }
                let weak = Rc::downgrade(&entry);
                // The view keeps the listener for its life.
                let _ = view.add_listener(move |data, result_type, _| {
                    if let Some(entry) = weak.upgrade() {
                        *entry.data.borrow_mut() = data.to_json().unwrap_or(Value::Null);
                        *entry.result_type.borrow_mut() = result_type.as_str().to_owned();
                        entry.notifications.set(entry.notifications.get() + 1);
                    }
                });
            }
            "destroy" => {
                let name = string("view")?;
                let view = self
                    .views
                    .borrow()
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| v.view.clone());
                if let Some(v) = view {
                    v.destroy();
                }
                self.views.borrow_mut().retain(|(n, _)| *n != name);
            }
            "updateTTL" => {
                let name = string("view")?;
                let view = self
                    .views
                    .borrow()
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| v.view.clone());
                if let (Some(v), Some(t)) = (view, ttl(step.get("ttl"))) {
                    v.update_ttl(&t);
                }
            }
            "preload" => {
                let q = request(step.str("query")?, step.get("args"))?;
                let p = self
                    .zero()
                    .preload(&q, &ttl(step.get("ttl")).unwrap_or(Ttl::None));
                let (cached, complete) = (pending(), pending());
                self.preloads.borrow_mut().push((
                    string("preload")?,
                    cached.clone(),
                    complete.clone(),
                    p.cleanup.clone(),
                ));
                p.cached
                    .on_settle(move |r| *cached.borrow_mut() = settled(r));
                p.complete
                    .on_settle(move |r| *complete.borrow_mut() = settled(r));
            }
            "cleanup" => {
                let name = string("preload")?;
                let cleanup = self
                    .preloads
                    .borrow()
                    .iter()
                    .find(|(n, ..)| *n == name)
                    .map(|(_, _, _, c)| c.clone());
                if let Some(c) = cleanup {
                    self.zero().dispatch(|| c());
                }
            }
            "run" => {
                let name = string("run")?;
                let entry = pending();
                {
                    let mut runs = self.runs.borrow_mut();
                    runs.retain(|(n, _)| *n != name);
                    runs.push((name, entry.clone()));
                }
                let q = request(step.str("query")?, step.get("args"))?;
                let run_type = RunType::parse(
                    step.get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown"),
                )
                .ok_or("Unknown run type")?;
                let result = self.zero().run(&q, run_type, None);
                result.on_settle(move |r| {
                    let mut o = Object::new();
                    match r {
                        Ok(data) => {
                            o.insert("data", data.to_json().unwrap_or(Value::Null));
                        }
                        Err(Error::Query(e)) => {
                            o.insert(
                                "rejected",
                                e.data
                                    .as_ref()
                                    .and_then(|d| d.to_json())
                                    .unwrap_or(Value::Null),
                            );
                        }
                        Err(_) => {
                            o.insert("rejected", Value::Object(Object::new()));
                        }
                    }
                    *entry.borrow_mut() = Value::Object(o);
                });
            }
            "mutate" => {
                let name = string("mutator")?;
                if !mutators().iter().any(|(n, _)| *n == name) {
                    return Err(format!("Unknown name {name}"));
                }
                let zero = self.zero();
                let result = zero.dispatch(|| zero.mutate(&name, step.get("args").cloned()));
                let (client, server) = (pending(), pending());
                self.mutations.borrow_mut().push((
                    string("mutation")?,
                    client.clone(),
                    server.clone(),
                ));
                result.client.on_settle(move |r| {
                    if let Ok(o) = r {
                        *client.borrow_mut() = o.to_json();
                    }
                });
                result.server.on_settle(move |r| {
                    if let Ok(o) = r {
                        *server.borrow_mut() = o.to_json();
                    }
                });
            }
            "receive" => {
                let text = json::stringify(&self.substitute(step.field("message")?));
                self.socket(step.get("socket"))?.receive_text(&text);
            }
            "receiveAll" => {
                let socket = self.socket(step.get("socket"))?;
                for m in step
                    .get("messages")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    socket.receive_text(&json::stringify(&self.substitute(m)));
                }
            }
            "receiveChunk" => {
                let payload: Vec<u8> =
                    if let Some(bytes) = step.get("bytes").and_then(Value::as_array) {
                        bytes
                            .iter()
                            .map(|b| b.as_f64().unwrap_or(0.0) as u8)
                            .collect()
                    } else if let Some(text) = step.get("text").and_then(Value::as_str) {
                        text.as_bytes().to_vec()
                    } else {
                        let parts = step.get("parts").cloned().unwrap_or(Value::Array(vec![]));
                        json::stringify(&self.substitute(&parts)).into_bytes()
                    };
                let mut bytes = vec![poke_chunk::MESSAGE_TYPE];
                bytes.extend(payload);
                self.socket(step.get("socket"))?
                    .receive(SocketMessage::Binary(bytes));
            }
            "receiveText" => {
                self.socket(step.get("socket"))?
                    .receive_text(step.str("text")?);
            }
            "receiveBinary" => {
                let bytes = step
                    .get("bytes")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|b| b.as_f64().unwrap_or(0.0) as u8)
                    .collect();
                self.socket(step.get("socket"))?
                    .receive(SocketMessage::Binary(bytes));
            }
            "socketClose" => {
                self.socket(step.get("socket"))?.server_close(
                    step.get("code").and_then(Value::as_f64).unwrap_or(1006.0) as u16,
                    step.get("reason").and_then(Value::as_str).unwrap_or(""),
                    step.get("wasClean")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                );
            }
            "advance" => self
                .scheduler
                .advance(step.get("ms").and_then(Value::as_f64).unwrap_or(0.0)),
            "visibility" => self
                .zero()
                .set_visible(step.get("state").and_then(Value::as_str) == Some("visible")),
            "onState" => {
                let name = string("state")?;
                let then = step.field("then")?.clone();
                let unsubscribe: Rc<RefCell<Option<Unsubscribe>>> = Rc::default();
                let w = self.me.clone();
                let u = unsubscribe.clone();
                let remove = self.zero().connection().subscribe(move |state| {
                    if state.status().as_str() != name {
                        return;
                    }
                    if let Some(r) = u.borrow_mut().take() {
                        r();
                    } else {
                        return;
                    }
                    let Some(runner) = w.upgrade() else { return };
                    let zero = runner.zero();
                    if then.get("op").and_then(Value::as_str) == Some("close") {
                        zero.close();
                    } else {
                        zero.connection()
                            .connect(then.get("auth").and_then(Value::as_str).map(str::to_owned));
                    }
                });
                *unsubscribe.borrow_mut() = Some(remove);
            }
            "connect" => self
                .zero()
                .connection()
                .connect(step.get("auth").and_then(Value::as_str).map(str::to_owned)),
            "close" => self.zero().close(),
            "restart" => self.restart(step)?,
            "listen" => {
                let name = string("view")?;
                let view = self
                    .views
                    .borrow()
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| v.view.clone())
                    .ok_or_else(|| format!("No view {name}"))?;
                let after = step.get("after").and_then(Value::as_f64).unwrap_or(1.0) as u32;
                let pending_actions =
                    RefCell::new(step.get("actions").and_then(Value::as_array).cloned());
                let entry = Rc::new(ListenerState::default());
                let listener = string("listener")?;
                {
                    let mut l = self.listeners.borrow_mut();
                    l.retain(|(n, _)| *n != listener);
                    l.push((listener, entry.clone()));
                }
                let count = Cell::new(0u32);
                let w = self.me.clone();
                let weak_entry = Rc::downgrade(&entry);
                let remove = view.add_listener(move |data, result_type, _| {
                    let Some(entry) = weak_entry.upgrade() else {
                        return;
                    };
                    let mut o = Object::new();
                    o.insert("data", data.to_json().unwrap_or(Value::Null));
                    o.insert("resultType", s(result_type.as_str()));
                    entry.calls.borrow_mut().push(Value::Object(o));
                    count.set(count.get() + 1);
                    if count.get() > after {
                        let actions = pending_actions.borrow_mut().take();
                        if let (Some(actions), Some(runner)) = (actions, w.upgrade()) {
                            for action in &actions {
                                if let Err(e) = runner.perform(action) {
                                    let mut threw = entry.threw.borrow_mut();
                                    if threw.is_none() {
                                        *threw = Some(e);
                                    }
                                }
                            }
                        }
                    }
                });
                *entry.remove.borrow_mut() = Some(remove);
            }
            "unlisten" => {
                let name = string("listener")?;
                let entry = self
                    .listeners
                    .borrow()
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, l)| l.clone());
                if let Some(entry) = entry
                    && let Some(r) = &*entry.remove.borrow()
                {
                    r();
                }
            }
            _ => return Err(format!("Unknown op {op}")),
        }
        Ok(())
    }

    /// Settles the work due now (`settle` in `harness.ts`).
    fn settle(&self) {
        self.zero().dispatch(|| {});
        self.scheduler.run_due_timers();
    }

    fn run(&self) -> Vec<Value> {
        let mut results = Vec::new();
        let steps = self
            .scenario
            .get("steps")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for step in &steps {
            let sockets = self.factory.sockets();
            let sent_counts: Vec<usize> = sockets.iter().map(|s| s.messages().len()).collect();
            let closed_before: Vec<bool> = sockets.iter().map(|s| s.closed()).collect();
            let mut result = Object::new();
            if let Err(e) = self.perform(step) {
                result.insert("threw", Value::String(e));
            }
            self.settle();

            let now = self.factory.sockets();
            let opened: Vec<Value> = now[sockets.len()..]
                .iter()
                .map(|s| {
                    let mut o = Object::new();
                    o.insert("url", self.socket_url(&s.url));
                    let protocol = s
                        .sec_protocol
                        .as_deref()
                        .and_then(|p| sec_protocols::decode(p).ok())
                        .unwrap_or(Value::Null);
                    o.insert("protocol", self.normalize(&protocol, None));
                    Value::Object(o)
                })
                .collect();
            if !opened.is_empty() {
                result.insert("opened", Value::Array(opened));
            }
            let mut sent = Vec::new();
            for (i, s) in now.iter().enumerate() {
                let from = sent_counts.get(i).copied().unwrap_or(0);
                for m in &s.messages()[from..] {
                    let v = json::parse(m).unwrap_or_else(|_| Value::String(m.clone()));
                    sent.push(self.normalize(&v, None));
                }
            }
            if !sent.is_empty() {
                result.insert("sent", Value::Array(sent));
            }
            let closed: Vec<Value> = now
                .iter()
                .enumerate()
                .filter(|(i, s)| s.closed() && !closed_before.get(*i).copied().unwrap_or(false))
                .map(|(i, _)| Value::from(i as f64))
                .collect();
            if !closed.is_empty() {
                result.insert("closed", Value::Array(closed));
            }
            result.insert(
                "state",
                self.normalize(&self.zero().connection().state().to_json(), None),
            );
            if !self.views.borrow().is_empty() {
                let mut o = Object::new();
                for (name, v) in self.views.borrow().iter() {
                    let mut e = Object::new();
                    e.insert("data", v.data.borrow().clone());
                    e.insert("resultType", Value::String(v.result_type.borrow().clone()));
                    e.insert(
                        "notifications",
                        Value::from(f64::from(v.notifications.get())),
                    );
                    v.notifications.set(0);
                    o.insert(name.clone(), Value::Object(e));
                }
                result.insert("views", Value::Object(o));
            }
            if !self.mutations.borrow().is_empty() {
                let mut o = Object::new();
                for (name, c, sv) in self.mutations.borrow().iter() {
                    let mut e = Object::new();
                    e.insert("client", c.borrow().clone());
                    e.insert("server", sv.borrow().clone());
                    o.insert(name.clone(), Value::Object(e));
                }
                result.insert("mutations", Value::Object(o));
            }
            if !self.runs.borrow().is_empty() {
                let mut o = Object::new();
                for (name, r) in self.runs.borrow().iter() {
                    o.insert(name.clone(), r.borrow().clone());
                }
                result.insert("runs", Value::Object(o));
            }
            if !self.preloads.borrow().is_empty() {
                let mut o = Object::new();
                for (name, c, p, _) in self.preloads.borrow().iter() {
                    let mut e = Object::new();
                    e.insert("cached", c.borrow().clone());
                    e.insert("complete", p.borrow().clone());
                    o.insert(name.clone(), Value::Object(e));
                }
                result.insert("preloads", Value::Object(o));
            }
            let events = std::mem::take(&mut *self.events.borrow_mut());
            if !events.is_empty() {
                result.insert("events", Value::Array(events));
            }
            if !self.listeners.borrow().is_empty() {
                let mut o = Object::new();
                for (name, l) in self.listeners.borrow().iter() {
                    let mut v = Object::new();
                    v.insert(
                        "calls",
                        Value::Array(std::mem::take(&mut *l.calls.borrow_mut())),
                    );
                    if let Some(threw) = l.threw.borrow_mut().take() {
                        v.insert("threw", Value::String(threw));
                    }
                    o.insert(name.clone(), Value::Object(v));
                }
                result.insert("listeners", Value::Object(o));
            }
            results.push(Value::Object(result));
        }
        self.zero().close();
        results
    }

    /// The connect URL with its query parameters in order (`socketURL`).
    fn socket_url(&self, url: &str) -> Value {
        let (base, query) = url.split_once('?').unwrap_or((url, ""));
        let scheme_end = base.find("://").map_or(0, |i| i + 3);
        let path_start = base[scheme_end..]
            .find('/')
            .map_or(base.len(), |i| scheme_end + i);
        let placeholders = self.placeholders.borrow();
        let mut pairs = Vec::new();
        for part in query.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = part.split_once('=').unwrap_or((part, ""));
            let (k, v) = (form_decode(k), form_decode(v));
            let value = match k.as_str() {
                "ts" => continue,
                "wsid" | "profileID" => format!("${k}"),
                _ => placeholders.get(&v).cloned().unwrap_or(v),
            };
            pairs.push(Value::Array(vec![Value::String(k), Value::String(value)]));
        }
        let mut o = Object::new();
        o.insert("origin", s(&base[..path_start]));
        o.insert("path", s(&base[path_start..]));
        o.insert("query", Value::Array(pairs));
        Value::Object(o)
    }
}

fn normalize(placeholders: &HashMap<String, String>, v: &Value, key: Option<&str>) -> Value {
    match v {
        Value::String(s) => {
            if key == Some("requestID") {
                return Value::from("$requestID");
            }
            Value::String(placeholders.get(s).cloned().unwrap_or_else(|| s.clone()))
        }
        Value::Array(a) => {
            Value::Array(a.iter().map(|e| normalize(placeholders, e, None)).collect())
        }
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, e)| {
                    (
                        placeholders.get(k).cloned().unwrap_or_else(|| k.clone()),
                        normalize(placeholders, e, Some(k)),
                    )
                })
                .collect(),
        ),
        v => v.clone(),
    }
}

fn substitute(ids: &HashMap<String, String>, v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(ids.get(s).cloned().unwrap_or_else(|| s.clone())),
        Value::Array(a) => Value::Array(a.iter().map(|e| substitute(ids, e)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, e)| {
                    (
                        ids.get(k).cloned().unwrap_or_else(|| k.clone()),
                        substitute(ids, e),
                    )
                })
                .collect(),
        ),
        v => v.clone(),
    }
}

/// `application/x-www-form-urlencoded` decoding, as `URLSearchParams`
/// reads it.
fn form_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 3 <= bytes.len() => {
                match std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    Some(b) => {
                        out.push(b);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
