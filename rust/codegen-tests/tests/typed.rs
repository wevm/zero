//! The generated Rust drives the M2 engine: typed queries build the ASTs
//! the untyped builder does (which the conformance vectors check against
//! JS), and their views, hydrated and pushed to, read as generated rows.

use std::collections::HashMap;
use std::rc::Rc;
use zero_client::builder::{BuilderDelegate, build_pipeline, group_view_input};
use zero_client::decode::JsonCodable;
use zero_client::ivm::{ArrayView, Input, MemorySource, Source, SourceChange};
use zero_client::json::{Object, Value};
use zero_client::typed::{
    AnyQuery, Direction, ExistsOptions, JsonPathSegment, Query, QueryValue, RelationshipAggregates,
    SimpleOperator, ZeroRow,
};
use zero_codegen_tests::test_schema::{
    Comment, Issue, IssueLabel, Label, User, UserMetadata, UserMetadataRegistrar, schema, zql,
};

fn issue(id: &str, title: &str, closed: bool, owner: Option<&str>, created: f64) -> Issue {
    Issue {
        id: id.into(),
        title: title.into(),
        description: String::new(),
        closed,
        owner_id: owner.map(Into::into),
        created_at: created,
        owner: None,
        comments: None,
        one_comment: None,
        owner_comments: None,
        labels: None,
        aggregates: RelationshipAggregates::default(),
    }
}

fn user(id: &str, name: &str, metadata: Option<UserMetadata>) -> User {
    User {
        id: id.into(),
        name: name.into(),
        metadata,
        issues: None,
        aggregates: RelationshipAggregates::default(),
    }
}

fn comment(id: &str, author: &str, issue: &str, text: &str, created: f64) -> Comment {
    Comment {
        id: id.into(),
        author_id: author.into(),
        issue_id: issue.into(),
        text: text.into(),
        created_at: created,
        issue: None,
        revisions: None,
        author: None,
        aggregates: RelationshipAggregates::default(),
    }
}

fn alice() -> User {
    user(
        "u1",
        "Alice",
        Some(UserMetadata {
            registrar: UserMetadataRegistrar::Github,
            email: "a@example.com".into(),
            login: None,
            alt_contacts: Some(vec!["a2@example.com".into()]),
        }),
    )
}

struct Sources(HashMap<String, Rc<MemorySource>>);

impl BuilderDelegate for Sources {
    fn get_source(&self, table: &str) -> Option<Rc<dyn Source>> {
        self.0.get(table).map(|s| s.clone() as Rc<dyn Source>)
    }
}

impl Sources {
    /// The sources of the test schema's tables, with the rows made from
    /// generated row structs.
    fn new() -> Self {
        let sources: HashMap<_, _> = schema()
            .tables
            .iter()
            .map(|(name, table)| (name.clone(), MemorySource::new(table.clone())))
            .collect();
        let s = Sources(sources);
        s.add(&[
            alice().column_values(),
            user("u2", "Bob", None).column_values(),
        ]);
        s.add(&[
            issue("i1", "First", false, Some("u1"), 1.0).column_values(),
            issue("i2", "Second", true, Some("u2"), 2.0).column_values(),
            issue("i3", "Third", false, None, 3.0).column_values(),
        ]);
        s.add(&[
            comment("c1", "u1", "i1", "hi", 10.0).column_values(),
            comment("c2", "u2", "i1", "yo", 20.0).column_values(),
            comment("c3", "u1", "i3", "ok", 5.0).column_values(),
        ]);
        s.add(&[Label {
            id: "l1".into(),
            name: "bug".into(),
            issues: None,
            aggregates: RelationshipAggregates::default(),
        }
        .column_values()]);
        s.add(&[IssueLabel {
            issue_id: "i1".into(),
            label_id: "l1".into(),
        }
        .column_values()]);
        s
    }

    fn table_of(row: &Object) -> &'static str {
        match () {
            _ if row.contains_key("labelId") => "issueLabel",
            _ if row.contains_key("issueId") => "comment",
            _ if row.contains_key("closed") => "issue",
            _ if row.contains_key("metadata") => "user",
            _ => "label",
        }
    }

    fn add(&self, rows: &[Object]) {
        for row in rows {
            self.push(SourceChange::Add(Rc::new(row.clone())));
        }
    }

    fn push(&self, change: SourceChange) {
        let row = match &change {
            SourceChange::Add(r) | SourceChange::Remove(r) | SourceChange::Edit { row: r, .. } => {
                r.clone()
            }
        };
        self.0[Self::table_of(&row)].push(change);
    }

    /// The view of `query`, hydrated.
    fn view<R, T>(&self, query: &Query<R, T>) -> Rc<ArrayView> {
        let ast = query.any().ast();
        let input = build_pipeline(ast, self, "q").expect("the pipeline builds");
        let input: Rc<dyn Input> = if ast.group_by.is_some() {
            group_view_input(input, ast, self).expect("the group view builds")
        } else {
            input
        };
        ArrayView::new(input, query.any().format().clone(), None, true)
    }

    /// What `query` materializes to, read as its typed result.
    fn read<R, T: QueryValue>(&self, query: &Query<R, T>) -> T {
        let view = self.view(query);
        query
            .read(&view.data())
            .expect("the view reads as the result")
    }
}

/// Each typed query builds the AST, format, and hash of the untyped
/// query with the same names, and is invalid for the same reason.
#[test]
fn typed_queries_build_the_untyped_queries() {
    let any = || AnyQuery::new(schema(), "issue", zero_client::ast::System::Client);
    let cases: Vec<(&str, AnyQuery, AnyQuery)> = vec![
        (
            "where_eq with None uses IS",
            zql::issue().where_eq(Issue::OWNER_ID, None).into_any(),
            any().where_("ownerId", SimpleOperator::Is, Value::Null),
        ),
        (
            "where_eq with a value uses =",
            zql::issue().where_eq(Issue::CLOSED, false).into_any(),
            any().where_("closed", SimpleOperator::Eq, false),
        ),
        (
            "where_ with an operator",
            zql::issue()
                .where_(Issue::CREATED_AT, SimpleOperator::Gt, 5.0)
                .into_any(),
            any().where_("createdAt", SimpleOperator::Gt, 5.0),
        ),
        (
            "where_in",
            zql::issue()
                .where_in(
                    Issue::ID,
                    SimpleOperator::NotIn,
                    vec!["a".into(), "b".into()],
                )
                .into_any(),
            any().where_(
                "id",
                SimpleOperator::NotIn,
                Value::Array(vec!["a".into(), "b".into()]),
            ),
        ),
        (
            "where_expr with or, like, ne None, and a JSON path",
            zql::issue()
                .where_expr(|e| {
                    e.or(vec![
                        Issue::TITLE.like("%x%"),
                        Issue::OWNER_ID.ne(None),
                        e.not(Issue::CREATED_AT.le(2.0)),
                    ])
                })
                .into_any(),
            any().where_expr(|eb| {
                eb.or(vec![
                    eb.cmp("title", SimpleOperator::Like, "%x%"),
                    eb.cmp("ownerId", SimpleOperator::IsNot, Value::Null),
                    eb.not(eb.cmp("createdAt", SimpleOperator::Le, 2.0)),
                ])
            }),
        ),
        (
            "where_exists over a junction, flipped",
            zql::issue()
                .where_exists(
                    Issue::LABELS,
                    ExistsOptions {
                        flip: Some(true),
                        scalar: None,
                    },
                    |q| q.where_eq(Label::NAME, "bug".into()),
                )
                .into_any(),
            any().where_exists(
                "labels",
                ExistsOptions {
                    flip: Some(true),
                    scalar: None,
                },
                |q| q.where_("name", SimpleOperator::Eq, "bug"),
            ),
        ),
        (
            "related with order, limit, and nested related",
            zql::issue()
                .related(Issue::COMMENTS, |q| {
                    q.order_by(Comment::CREATED_AT, Direction::Desc)
                        .limit(2)
                        .related(Comment::AUTHOR, |q| q)
                })
                .related(Issue::OWNER_COMMENTS, |q| q.count())
                .order_by(Issue::TITLE, Direction::Asc)
                .into_any(),
            any()
                .related("comments", |q| {
                    q.order_by("createdAt", Direction::Desc)
                        .limit(2)
                        .related("author", |q| q)
                })
                .related("ownerComments", |q| q.count())
                .order_by("title", Direction::Asc),
        ),
        (
            "start after a row",
            zql::issue()
                .order_by(Issue::CREATED_AT, Direction::Asc)
                .start(&issue("i2", "Second", true, Some("u2"), 2.0), true)
                .into_any(),
            any().order_by("createdAt", Direction::Asc).start(
                issue("i2", "Second", true, Some("u2"), 2.0).column_values(),
                true,
            ),
        ),
        (
            "start at column values",
            zql::issue()
                .start_values(
                    vec![
                        (Issue::CREATED_AT.into(), 2.0.into()),
                        (Issue::ID.into(), "i2".into()),
                    ],
                    false,
                )
                .into_any(),
            any().start(
                [("createdAt", Value::from(2.0)), ("id", "i2".into())]
                    .into_iter()
                    .collect(),
                false,
            ),
        ),
        ("one", zql::issue().one().into_any(), any().one()),
        (
            "aggregates",
            zql::issue().max(Issue::CREATED_AT).into_any(),
            any().max("createdAt"),
        ),
        (
            "sum of a JSON path",
            zql::issue()
                .sum_json(Issue::DESCRIPTION, vec![JsonPathSegment::Key("n".into())])
                .into_any(),
            any().sum_json("description", vec![JsonPathSegment::Key("n".into())]),
        ),
        (
            "group_by",
            zql::issue()
                .group_by(vec![Issue::OWNER_ID.into(), Issue::CLOSED.into()])
                .count()
                .into_any(),
            any()
                .group_by(vec!["ownerId".into(), "closed".into()])
                .count(),
        ),
        (
            "distinct",
            zql::issue()
                .distinct(vec![Issue::OWNER_ID.into()])
                .into_any(),
            any().distinct(vec!["ownerId".into()]),
        ),
        (
            "a negative limit is invalid",
            zql::issue().limit(-1).into_any(),
            any().limit(-1),
        ),
    ];
    for (name, typed, untyped) in cases {
        assert_eq!(
            typed.ast().to_json(),
            untyped.ast().to_json(),
            "{name}: AST"
        );
        assert_eq!(
            typed.format().to_json(),
            untyped.format().to_json(),
            "{name}: format"
        );
        assert_eq!(typed.hash(), untyped.hash(), "{name}: hash");
        assert_eq!(typed.invalid(), untyped.invalid(), "{name}: invalid");
    }
    assert_eq!(
        zql::issue().limit(-1).any().invalid(),
        Some("Limit must be non-negative")
    );
}

#[test]
fn views_read_as_generated_rows_and_follow_pushes() {
    let sources = Sources::new();
    let query = zql::issue()
        .where_eq(Issue::CLOSED, false)
        .related(Issue::OWNER, |q| q)
        .related(Issue::COMMENTS, |q| {
            q.order_by(Comment::CREATED_AT, Direction::Desc).limit(1)
        })
        .related(Issue::OWNER_COMMENTS, |q| q.count())
        .order_by(Issue::CREATED_AT, Direction::Asc);
    let view = sources.view(&query);
    let issues = query.read(&view.data()).unwrap();

    let expected_first = Issue {
        owner: Some(Box::new(alice())),
        comments: Some(vec![comment("c2", "u2", "i1", "yo", 20.0)]),
        ..issue("i1", "First", false, Some("u1"), 1.0)
    };
    let expected_third = Issue {
        comments: Some(vec![comment("c3", "u1", "i3", "ok", 5.0)]),
        ..issue("i3", "Third", false, None, 3.0)
    };
    assert_eq!(issues.len(), 2);
    // u1 wrote c1 and c3; the third issue has no owner, so none.
    assert_eq!(issues[0].aggregates.count(Issue::OWNER_COMMENTS), Some(2));
    assert_eq!(issues[1].aggregates.count(Issue::OWNER_COMMENTS), Some(0));
    assert_eq!(
        issues[0],
        Issue {
            aggregates: issues[0].aggregates.clone(),
            ..expected_first.clone()
        }
    );
    assert_eq!(
        issues[1],
        Issue {
            aggregates: issues[1].aggregates.clone(),
            ..expected_third
        }
    );
    // A row's columns are what the view holds.
    for (row, entry) in issues.iter().zip(view.data().entries()) {
        assert_eq!(row.column_values(), *entry.row);
    }

    // A newer comment by u1 on the first issue, and the third closed.
    sources.push(SourceChange::Add(Rc::new(
        comment("c4", "u1", "i1", "new", 30.0).column_values(),
    )));
    sources.push(SourceChange::Edit {
        row: Rc::new(issue("i3", "Third", true, None, 3.0).column_values()),
        old_row: Rc::new(issue("i3", "Third", false, None, 3.0).column_values()),
    });
    view.flush();
    let issues = query.read(&view.data()).unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].aggregates.count(Issue::OWNER_COMMENTS), Some(3));
    assert_eq!(
        issues[0],
        Issue {
            comments: Some(vec![comment("c4", "u1", "i1", "new", 30.0)]),
            aggregates: issues[0].aggregates.clone(),
            ..expected_first
        }
    );
}

#[test]
fn one_reads_a_row_or_none() {
    let sources = Sources::new();
    let alice_query = zql::user().where_eq(User::ID, "u1".into()).one();
    assert_eq!(sources.read(&alice_query), Some(alice()));
    let bob = sources.read(&zql::user().where_eq(User::ID, "u2".into()).one());
    assert_eq!(bob.map(|b| b.metadata), Some(None));
    let nobody = zql::user().where_eq(User::ID, "u9".into()).one();
    assert_eq!(sources.read(&nobody), None);
}

#[test]
fn filters_read_the_rows_they_keep() {
    let sources = Sources::new();
    let ids = |issues: Vec<Issue>| issues.into_iter().map(|i| i.id).collect::<Vec<_>>();
    let bugs = zql::issue().where_exists(Issue::LABELS, ExistsOptions::default(), |q| {
        q.where_eq(Label::NAME, "bug".into())
    });
    assert_eq!(ids(sources.read(&bugs)), ["i1"]);
    let unowned_or_late = zql::issue()
        .where_expr(|e| e.or(vec![Issue::OWNER_ID.eq(None), Issue::CREATED_AT.ge(2.0)]));
    assert_eq!(ids(sources.read(&unowned_or_late)), ["i2", "i3"]);
    let titled = zql::issue().where_expr(|e| e.and(vec![Issue::TITLE.ilike("%IR%")]));
    assert_eq!(ids(sources.read(&titled)), ["i1", "i3"]);
    let github = zql::user().where_expr(|e| {
        e.json(
            User::METADATA,
            vec![JsonPathSegment::Key("registrar".into())],
        )
        .eq("github".into())
    });
    let users: Vec<User> = sources.read(&github);
    assert_eq!(users, [alice()]);
}

#[test]
fn aggregates_read_as_their_values() {
    let sources = Sources::new();
    assert_eq!(sources.read(&zql::issue().count()), 3);
    assert_eq!(
        sources.read(&zql::issue().max(Issue::CREATED_AT)),
        Some(3.0)
    );
    assert_eq!(
        sources.read(&zql::issue().min(Issue::TITLE)),
        Some("First".to_owned())
    );
    let none = zql::issue().where_eq(Issue::ID, "none".into());
    assert_eq!(sources.read(&none.clone().sum(Issue::CREATED_AT)), None);
    assert_eq!(sources.read(&none.count()), 0);

    let by_author = sources.read(
        &zql::comment()
            .group_by(vec![Comment::AUTHOR_ID.into()])
            .count(),
    );
    let groups: Vec<(String, i64)> = by_author
        .iter()
        .map(|g| (g.get(Comment::AUTHOR_ID).unwrap(), g.value))
        .collect();
    assert_eq!(groups, [("u1".to_owned(), 2), ("u2".to_owned(), 1)]);

    let issues = sources.read(&zql::comment().distinct(vec![Comment::ISSUE_ID.into()]));
    let issues: Vec<String> = issues
        .iter()
        .map(|d| d.get(Comment::ISSUE_ID).unwrap())
        .collect();
    assert_eq!(issues, ["i1", "i3"]);
}

/// A column a row can't hold fails to read, naming it; a null where the
/// schema says there is none reads as the type's default, as JS shows it.
#[test]
fn rows_read_strictly_but_forgive_nulls() {
    let sources = Sources::new();
    let mut drifted = issue("i9", "Drifted", false, None, 9.0).column_values();
    drifted.insert("title", Value::Null);
    sources.add(&[drifted]);
    let rows = sources.read(&zql::issue().where_eq(Issue::ID, "i9".into()));
    assert_eq!(rows[0].title, "");

    let mut bad = alice().column_values();
    bad.insert("id", "u3".into());
    bad.insert("metadata", Value::from("not an object"));
    sources.add(&[bad]);
    let query = zql::user().where_eq(User::ID, "u3".into());
    let view = sources.view(&query);
    let error = query.read(&view.data()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "metadata: expected a UserMetadata object, got string \"not an object\""
    );
}
