//! The whole client, on the tokio transport, against zero-cache, the API
//! server, and Postgres (`packages/zero-conformance/src/e2e/`), as Swift's
//! `ClientE2ETests` and `AggregateE2ETests`: sync, optimistic mutations the
//! server confirms, a relaunch on the same store, two clients of one group,
//! and synced aggregates. `ZERO_E2E_URL` is zero-cache's `ws://` origin;
//! without it the tests do nothing.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::task::LocalSet;
use zero_client::ast::{Direction, SimpleOperator, System};
use zero_client::client::{
    MaterializedView, MutationOutcome, Mutator, RunType, Transaction, Zero, ZeroOptions,
    ZeroStorage,
};
use zero_client::ivm::{ResultType, ViewValue};
use zero_client::json::{self, Object, Value};
use zero_client::query::AnyQuery;
use zero_client::schema::Schema;
use zero_client::tokio_transport::{TokioScheduler, TokioSocketFactory};
use zero_client::ttl::Ttl;

/// The zql test schema the e2e app is written against, as JS wrote it into
/// `ivm.json`'s shared fixtures.
fn schema() -> Arc<Schema> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packages/zero-conformance/vectors/ivm.json"
    );
    let file = json::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
    let schema = file.get("shared").and_then(|s| s.get("schema")).unwrap();
    Arc::new(Schema::from_json(schema).unwrap())
}

fn unique(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{prefix}-{nanos:x}")
}

/// `issue.create` and `issue.close` of `app.ts`, as an app's Rust client
/// would define them.
fn mutators() -> Vec<Mutator<()>> {
    vec![
        Mutator::new("issue.create", |tx: &Transaction, args, _| {
            let args = args.unwrap();
            let mut row = Object::new();
            row.insert("id", args.get("id").unwrap().clone());
            row.insert("title", args.get("title").unwrap().clone());
            row.insert("description", Value::from(""));
            row.insert("closed", Value::from(false));
            row.insert(
                "ownerId",
                args.get("ownerId").cloned().unwrap_or(Value::Null),
            );
            row.insert("createdAt", Value::from(100.0));
            tx.table("issue").insert(&row)?;
            Ok(None)
        }),
        Mutator::new("issue.close", |tx: &Transaction, args, _| {
            let mut row = Object::new();
            row.insert("id", args.unwrap().clone());
            row.insert("closed", Value::from(true));
            tx.table("issue").update(&row)?;
            Ok(None)
        }),
    ]
}

struct E2e {
    origin: String,
    schema: Arc<Schema>,
    storage: ZeroStorage,
}

impl E2e {
    fn new() -> Option<E2e> {
        let Ok(url) = std::env::var("ZERO_E2E_URL") else {
            eprintln!("ZERO_E2E_URL is not set: skipping the client end-to-end tests");
            return None;
        };
        Some(E2e {
            origin: url.replacen("ws://", "http://", 1),
            schema: schema(),
            storage: ZeroStorage::Memory,
        })
    }

    fn zero(&self, storage_key: &str) -> Zero {
        Zero::new(
            self.schema.clone(),
            (),
            mutators(),
            ZeroOptions {
                cache_url: Some(self.origin.clone()),
                storage_key: Some(storage_key.to_owned()),
                storage: self.storage.clone(),
                ..ZeroOptions::default()
            },
            Rc::new(TokioScheduler::new()),
            Some(Rc::new(TokioSocketFactory::new())),
        )
        .unwrap()
    }

    fn zql(&self, table: &str) -> AnyQuery {
        AnyQuery::new(self.schema.clone(), table, System::Client)
    }

    /// `issues.open`.
    fn open(&self) -> AnyQuery {
        self.zql("issue")
            .where_("closed", SimpleOperator::Eq, false)
            .related("comments", |q| q)
            .related("owner", |q| q)
            .order_by("createdAt", Direction::Asc)
            .name_and_args("issues.open", Vec::new())
    }

    /// `issues.byID`.
    fn by_id(&self, id: &str) -> AnyQuery {
        self.zql("issue")
            .where_("id", SimpleOperator::Eq, id)
            .related("comments", |q| q)
            .one()
            .name_and_args("issues.byID", vec![Value::from(id)])
    }
}

fn create(zero: &Zero, id: &str, title: &str, owner: Option<&str>) -> MutationOutcomes {
    let mut args = Object::new();
    args.insert("id", Value::from(id));
    args.insert("title", Value::from(title));
    if let Some(o) = owner {
        args.insert("ownerId", Value::from(o));
    }
    let r = zero.mutate("issue.create", Some(Value::Object(args)));
    MutationOutcomes(r.client, r.server)
}

fn close(zero: &Zero, id: &str) -> MutationOutcomes {
    let r = zero.mutate("issue.close", Some(Value::from(id)));
    MutationOutcomes(r.client, r.server)
}

struct MutationOutcomes(
    zero_client::client::Deferred<MutationOutcome>,
    zero_client::client::Deferred<MutationOutcome>,
);

impl MutationOutcomes {
    async fn client(&self) -> MutationOutcome {
        self.0.clone().await.unwrap()
    }
    async fn server(&self) -> MutationOutcome {
        self.1.clone().await.unwrap()
    }
}

const SUCCESS: MutationOutcome = MutationOutcome::Success { data: None };

async fn eventually(what: &str, mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

fn rows(data: &ViewValue) -> Vec<Value> {
    data.to_json()
        .map(|v| match v {
            Value::Array(a) => a,
            v => vec![v],
        })
        .unwrap_or_default()
}

fn ids(data: &ViewValue) -> Vec<String> {
    rows(data)
        .iter()
        .filter_map(|r| r.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect()
}

fn row<'a>(rows: &'a [Value], id: &str) -> Option<&'a Value> {
    rows.iter()
        .find(|r| r.get("id").and_then(Value::as_str) == Some(id))
}

fn contains_all(ids: &[String], wanted: &[&str]) -> bool {
    wanted.iter().all(|w| ids.iter().any(|i| i == w))
}

async fn in_local_set(body: impl std::future::Future<Output = ()>) {
    tokio::time::timeout(Duration::from_secs(120), LocalSet::new().run_until(body))
        .await
        .expect("the test finished within 2 minutes");
}

#[tokio::test]
async fn syncs_mutates_and_relaunches() {
    let Some(e) = E2e::new() else { return };
    syncs_mutates_and_relaunches_on(e).await;
}

/// The same on SQLite storage: the relaunch reads the rows from the file.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn syncs_mutates_and_relaunches_on_sqlite() {
    let Some(mut e) = E2e::new() else { return };
    let directory = std::env::temp_dir().join(unique("zero-e2e-sqlite"));
    e.storage = ZeroStorage::Sqlite {
        directory: Some(directory.clone()),
    };
    syncs_mutates_and_relaunches_on(e).await;
    let _ = std::fs::remove_dir_all(directory);
}

async fn syncs_mutates_and_relaunches_on(e: E2e) {
    in_local_set(async {
        let key = unique("rust-key");
        let zero = e.zero(&key);
        let open = zero.materialize(&e.open(), &Ttl::default()).unwrap();
        eventually("the open issues", || {
            open.result_type() == ResultType::Complete
        })
        .await;
        let data = rows(&open.data());
        assert!(contains_all(&ids(&open.data()), &["i1", "i3", "i4"]));
        let i1 = row(&data, "i1").unwrap();
        let comments: Vec<_> = i1
            .get("comments")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.get("id").unwrap().as_str().unwrap().to_owned())
            .collect();
        assert_eq!(comments, ["c1", "c2"]);
        assert_eq!(
            i1.get("owner")
                .and_then(|o| o.get("name"))
                .and_then(Value::as_str),
            Some("Alice")
        );
        assert_eq!(
            row(&data, "i3")
                .and_then(|r| r.get("title"))
                .and_then(Value::as_str),
            Some("Emoji 😀 in titles")
        );

        // Optimistic at once, then confirmed by the server.
        let id = unique("rust");
        let created = create(&zero, &id, "From Rust", Some("u2"));
        assert!(ids(&open.data()).contains(&id));
        assert_eq!(created.client().await, SUCCESS);
        assert_eq!(created.server().await, SUCCESS);
        let confirmed = zero
            .run(&e.by_id(&id), RunType::Complete, None)
            .await
            .unwrap();
        assert_eq!(
            rows(&confirmed)[0].get("title").and_then(Value::as_str),
            Some("From Rust")
        );

        // Another client sees it through the server.
        let other = e.zero(&unique("rust-key"));
        let seen = other
            .run(&e.by_id(&id), RunType::Complete, None)
            .await
            .unwrap();
        assert_eq!(
            rows(&seen)[0].get("ownerId").and_then(Value::as_str),
            Some("u2")
        );

        let closed = close(&zero, &id);
        assert_eq!(closed.server().await, SUCCESS);
        assert!(!ids(&open.data()).contains(&id));
        let live_other = other.materialize(&e.by_id(&id), &Ttl::default()).unwrap();
        eventually("the close to sync", || {
            rows(&live_other.data())
                .first()
                .and_then(|r| r.get("closed"))
                == Some(&Value::from(true))
        })
        .await;
        other.close();

        // A relaunch reads the rows from its store before the server
        // answers: memory in the same process, as JS `kvStore: 'mem'`
        // does, or the SQLite file.
        let group = zero.client_group_id().to_owned();
        zero.close();
        drop(zero);
        let relaunched = e.zero(&key);
        assert_eq!(relaunched.client_group_id(), group);
        let cached = relaunched
            .run(&e.open(), RunType::Cached, None)
            .await
            .unwrap();
        let cached = ids(&cached);
        assert!(contains_all(&cached, &["i1", "i3", "i4"]), "{cached:?}");
        assert!(!cached.contains(&id));
        relaunched.close();
    })
    .await;
}

/// Two clients of one client group (two windows) both keep their queries:
/// each lists the other in `activeClients`, so zero-cache doesn't delete
/// it.
#[tokio::test]
async fn two_clients_of_a_group_both_keep_their_queries() {
    let Some(e) = E2e::new() else { return };
    in_local_set(async {
        let key = unique("rust-key");
        let first = e.zero(&key);
        // Without a TTL, a query of a client the server deletes goes at once.
        let open = first.materialize(&e.open(), &Ttl::None).unwrap();
        eventually("the first client's issues", || {
            open.result_type() == ResultType::Complete
        })
        .await;

        let second = e.zero(&key);
        assert_eq!(second.client_group_id(), first.client_group_id());
        let by_id = second.materialize(&e.by_id("i1"), &Ttl::default()).unwrap();
        eventually("the second client's issue", || {
            by_id.result_type() == ResultType::Complete
        })
        .await;

        // The first client's query still syncs a row only it asks for.
        let id = unique("rust");
        let created = create(&second, &id, "Two windows", None);
        assert_eq!(created.server().await, SUCCESS);
        eventually("the first client to see the new issue", || {
            ids(&open.data()).contains(&id)
        })
        .await;
        assert_eq!(open.result_type(), ResultType::Complete);
        assert_eq!(ids(&by_id.data()), ["i1"]);
        assert_eq!(close(&second, &id).server().await, SUCCESS);
        first.close();
        second.close();
    })
    .await;
}

/// The aggregates zero-cache computes and streams (`aggregates` in
/// `app.ts`), and a grouped count moved optimistically by a mutation and
/// then confirmed.
#[tokio::test]
async fn syncs_aggregates_and_moves_them_optimistically() {
    let Some(e) = E2e::new() else { return };
    in_local_set(async {
        let zero = e.zero(&unique("rust-key"));
        let view = |q: AnyQuery| -> MaterializedView { zero.materialize(&q, &Ttl::default()).unwrap() };
        let count = view(e.zql("comment").count().name_and_args("aggregates.commentCount", vec![]));
        let per_issue = view(
            e.zql("issue")
                .where_(
                    "id",
                    SimpleOperator::In,
                    Value::Array(["i1", "i2", "i3", "i4"].map(Value::from).to_vec()),
                )
                .related("comments", |q| q.count())
                .order_by("id", Direction::Asc)
                .name_and_args("aggregates.commentCounts", vec![]),
        );
        let by_author = view(
            e.zql("comment")
                .group_by(vec!["authorId".to_owned()])
                .count()
                .name_and_args("aggregates.commentsByAuthor", vec![]),
        );
        // Before the server answers, a count is 0.
        assert_eq!(count.data(), ViewValue::Value(Value::from(0.0)));
        eventually("the aggregates", || {
            [&count, &per_issue, &by_author]
                .iter()
                .all(|v| v.result_type() == ResultType::Complete)
        })
        .await;
        assert_eq!(count.data(), ViewValue::Value(Value::from(3.0)));
        let counts: Vec<_> = rows(&per_issue.data())
            .iter()
            .map(|r| r.get("comments").cloned().unwrap())
            .collect();
        assert_eq!(counts, [2.0, 0.0, 1.0, 0.0].map(Value::from));
        assert_eq!(
            json::stringify(&by_author.data().to_json().unwrap()),
            r#"[{"authorId":"u1","count":1},{"authorId":"u2","count":1},{"authorId":"u3","count":1}]"#
        );

        // A grouped count moves with the mutation at once; a top-level one
        // when the server confirms it.
        let title = unique("aggregates");
        let titled = view(
            e.zql("issue")
                .where_("title", SimpleOperator::Eq, title.as_str())
                .count()
                .name_and_args("aggregates.titled", vec![Value::from(title.as_str())]),
        );
        let owners = view(
            e.zql("issue")
                .where_("title", SimpleOperator::Eq, title.as_str())
                .group_by(vec!["ownerId".to_owned()])
                .count()
                .name_and_args("aggregates.titledOwners", vec![Value::from(title.as_str())]),
        );
        eventually("the titled aggregates", || {
            titled.result_type() == ResultType::Complete
                && owners.result_type() == ResultType::Complete
        })
        .await;
        assert_eq!(titled.data(), ViewValue::Value(Value::from(0.0)));
        assert!(rows(&owners.data()).is_empty());
        let id = unique("rust");
        let created = create(&zero, &id, &title, Some("u2"));
        let owners_json = || json::stringify(&owners.data().to_json().unwrap());
        assert_eq!(owners_json(), r#"[{"ownerId":"u2","count":1}]"#);
        assert_eq!(titled.data(), ViewValue::Value(Value::from(0.0)));
        assert_eq!(created.server().await, SUCCESS);
        eventually("the server's count", || {
            titled.data() == ViewValue::Value(Value::from(1.0))
        })
        .await;
        assert_eq!(owners_json(), r#"[{"ownerId":"u2","count":1}]"#);
        // Closed, as the other tests leave theirs, so the fixture's open
        // issues stay the ones the walking skeleton expects.
        assert_eq!(close(&zero, &id).server().await, SUCCESS);
        zero.close();
    })
    .await;
}
