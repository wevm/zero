//! Typed named queries and mutators (`define`): their arguments go out as
//! the JSON JS sends, the client's context reaches their bodies, and a
//! mutator reads its arguments back before it runs.

use std::rc::Rc;
use std::sync::Arc;
use zero_client::ast::System;
use zero_client::client::{MutationOutcome, RunType, Transaction, Zero, ZeroOptions};
use zero_client::define::{
    MutatorDefinition, NamedMutator, NamedQuery, QueryDefinition, define_mutator, define_query,
};
use zero_client::json::{self, Object, Value};
use zero_client::query::{AnyQuery, ZeroQuery};
use zero_client::schema::Schema;
use zero_client::testing::{ManualScheduler, RecordingLogSink};
use zero_client::ttl::Ttl;
use zero_client::typed::{Query, SimpleOperator};

fn schema() -> Arc<Schema> {
    let schema = json::parse(
        r#"{"tables": {"issue": {"name": "issue", "columns": {
            "id": {"type": "string"}, "title": {"type": "string"},
            "owner": {"type": "string"}}, "primaryKey": ["id"]}}}"#,
    )
    .unwrap();
    Arc::new(Schema::from_json(&schema).unwrap())
}

/// Who is logged in.
type Context = String;

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ByTitle {
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<f64>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Create {
    id: String,
    title: String,
}

fn issues() -> Query<()> {
    Query::from_any(AnyQuery::new(schema(), "issue", System::Client))
}

/// The logged-in user's issues with the title.
const MINE_BY_TITLE: QueryDefinition<ByTitle, Context, ()> =
    define_query("issues.mineByTitle", |args, owner| {
        let q = issues().into_any();
        let q = q.where_("title", SimpleOperator::Eq, args.title.as_str());
        Query::from_any(q.where_("owner", SimpleOperator::Eq, owner.as_str()))
    });

const ALL: QueryDefinition<(), Context, ()> = define_query("issues.all", |(), _| issues());

/// Creates an issue owned by the logged-in user, and returns its id.
const CREATE: MutatorDefinition<Create, Context, String> =
    define_mutator("issue.create", |tx: &Transaction, args, owner| {
        let mut row = Object::new();
        row.insert("id", Value::from(args.id.as_str()));
        row.insert("title", Value::from(args.title));
        row.insert("owner", Value::from(owner.as_str()));
        tx.table("issue").insert(&row)?;
        Ok(args.id)
    });

const CLEAR: MutatorDefinition<(), Context> = define_mutator("issue.clear", |_, (), _| Ok(()));

fn client(owner: &str) -> (Zero<Context>, Rc<ManualScheduler>) {
    let scheduler = ManualScheduler::new(1_700_000_000_000.0);
    let zero = Zero::new(
        schema(),
        owner.to_owned(),
        vec![CREATE.mutator(), CLEAR.mutator()],
        ZeroOptions {
            log_sink: Some(Rc::new(RecordingLogSink::default())),
            // Memory stores outlive a client by name: each user gets one.
            storage_key: Some(owner.to_owned()),
            ..ZeroOptions::default()
        },
        scheduler.clone(),
        None,
    )
    .unwrap();
    scheduler.run_checkpoints();
    (zero, scheduler)
}

#[test]
fn a_query_request_sends_its_name_and_args_and_builds_with_the_context() {
    let request = MINE_BY_TITLE.with(ByTitle {
        title: "Bug".into(),
        limit: None,
    });
    // As JS sends `{title: 'Bug'}`: fields in declaration order, an
    // optional one left out.
    assert_eq!(
        json::stringify(&Value::Array(request.args().to_vec())),
        r#"[{"title":"Bug"}]"#
    );
    let any = request.any_query(&"u1".to_owned()).into_owned();
    let id = any.custom_query_id().unwrap();
    assert_eq!(
        (id.name.as_str(), id.args.as_slice()),
        ("issues.mineByTitle", request.args())
    );

    // No arguments sends none, as JS's `undefined`.
    assert!(ALL.with(()).args().is_empty());
}

#[test]
fn views_and_mutators_get_the_clients_context() {
    let (zero, scheduler) = client("u1");
    let mine = MINE_BY_TITLE.with(ByTitle {
        title: "Bug".into(),
        limit: None,
    });
    let view = zero.materialize(&mine, &Ttl::default()).unwrap();
    let result = zero.mutate_request(CREATE.with(Create {
        id: "i1".into(),
        title: "Bug".into(),
    }));
    scheduler.run_checkpoints();
    // The body's return value is the client outcome.
    assert_eq!(
        result.client.result().unwrap().unwrap(),
        MutationOutcome::Success {
            data: Some(Value::from("i1"))
        }
    );
    let rows = view.data();
    assert_eq!(rows.entries().len(), 1);
    assert_eq!(rows.entries()[0].row.get("owner"), Some(&Value::from("u1")));

    // `run` and `preload` take the request too; another user's view of the
    // same title is empty.
    let run = zero.run(&mine, RunType::Unknown, None);
    scheduler.run_checkpoints();
    assert_eq!(run.result().unwrap().unwrap().entries().len(), 1);
    let (other, s2) = client("u2");
    other.mutate_request(CREATE.with(Create {
        id: "i2".into(),
        title: "Bug".into(),
    }));
    s2.run_checkpoints();
    let theirs = other.run(&mine, RunType::Unknown, None);
    s2.run_checkpoints();
    let theirs = theirs.result().unwrap().unwrap();
    assert_eq!(theirs.entries()[0].row.get("id"), Some(&Value::from("i2")));
}

#[test]
fn a_mutator_reads_its_arguments_back_before_it_runs() {
    let (zero, scheduler) = client("u1");
    // A mutation replayed or sent by name with arguments that don't read as
    // the definition's type fails, as an app error, and writes nothing.
    let bad = zero.mutate("issue.create", Some(json::parse(r#"{"id": 1}"#).unwrap()));
    scheduler.run_checkpoints();
    match bad.client.result().unwrap().unwrap() {
        MutationOutcome::AppError { message, .. } => {
            assert!(message.contains("invalid type"), "{message}");
        }
        other => panic!("expected an app error, got {other:?}"),
    }
    // A mutator without arguments sends none and returns nothing.
    let clear = CLEAR.with(());
    assert_eq!(clear.args, None);
    let cleared = zero.mutate_request(clear);
    scheduler.run_checkpoints();
    assert_eq!(
        cleared.client.result().unwrap().unwrap(),
        MutationOutcome::Success { data: None }
    );
}

#[test]
fn definitions_take_json_arguments_as_the_server_gets_them() {
    let ctx = "u1".to_owned();
    let args = vec![json::parse(r#"{"limit": 3, "title": "Bug"}"#).unwrap()];
    let (query, sent) = MINE_BY_TITLE.query_from_json(&args, &ctx).unwrap();
    assert_eq!(
        json::stringify(&Value::Array(sent)),
        r#"[{"title":"Bug","limit":3}]"#
    );
    assert_eq!(query.custom_query_id().unwrap().name, "issues.mineByTitle");
    assert!(MINE_BY_TITLE.query_from_json(&[], &ctx).is_err());
    assert!(ALL.query_from_json(&[], &ctx).unwrap().1.is_empty());
    assert!(ALL.query_from_json(&[Value::from(1.0)], &ctx).is_err());

    let mutators: Vec<Box<dyn NamedMutator<Context>>> = vec![Box::new(CREATE), Box::new(CLEAR)];
    let names: Vec<_> = mutators.iter().map(|m| m.name()).collect();
    assert_eq!(names, ["issue.create", "issue.clear"]);
    let create = json::parse(r#"{"title": "t", "id": "i1"}"#).unwrap();
    assert_eq!(
        json::stringify(&mutators[0].reencode(Some(&create)).unwrap().unwrap()),
        r#"{"id":"i1","title":"t"}"#
    );
    assert_eq!(mutators[1].reencode(None).unwrap(), None);
}
