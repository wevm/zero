//! zbugs' first sync: the tables its issue list reads, with the
//! relationships of `apps/zbugs/shared/schema.ts`, and the rows of one poke
//! as zero-cache sent them to the zbugs-swift app (`FirstSync.swift`, which
//! generates the same rows from the same seed).

use std::collections::HashSet;
use std::sync::Arc;
use zero_client::ast::{Direction, SimpleOperator, System};
use zero_client::json::{self, Object, Value};
use zero_client::query::{AnyQuery, ExistsOptions};
use zero_client::schema::Schema;

/// The rows of the profiled first sync, by table.
const ISSUES: usize = 516;
const COMMENTS: usize = 894;
const ISSUE_LABELS: usize = 603;
const LABELS: usize = 30;
const USERS: usize = 14;
/// How many parts the poke came in.
pub const PARTS: usize = 21;
pub const PAGE_SIZE: usize = 50;
const USER_ID: &str = "user0";

pub fn schema() -> Arc<Schema> {
    fn table(name: &str, columns: &[(&str, &str, bool)], primary_key: &[&str]) -> String {
        let columns: Vec<String> = columns
            .iter()
            .map(|(c, t, optional)| format!(r#""{c}": {{"type": "{t}", "optional": {optional}}}"#))
            .collect();
        let pk: Vec<String> = primary_key.iter().map(|k| format!("\"{k}\"")).collect();
        format!(
            r#""{name}": {{"name": "{name}", "columns": {{{}}}, "primaryKey": [{}]}}"#,
            columns.join(","),
            pk.join(",")
        )
    }
    fn hop(source: &str, dest: &str, table: &str, cardinality: &str) -> String {
        format!(
            r#"{{"sourceField": ["{source}"], "destField": ["{dest}"], "destSchema": "{table}", "cardinality": "{cardinality}"}}"#
        )
    }
    fn relationships(table: &str, rels: &[(&str, Vec<String>)]) -> String {
        let rels: Vec<String> = rels
            .iter()
            .map(|(name, hops)| format!(r#""{name}": [{}]"#, hops.join(",")))
            .collect();
        format!(r#""{table}": {{{}}}"#, rels.join(","))
    }
    let (s, n, b) = ("string", "number", "boolean");
    let tables = [
        table(
            "user",
            &[
                ("id", s, false),
                ("login", s, false),
                ("name", s, true),
                ("avatar", s, false),
                ("role", s, false),
            ],
            &["id"],
        ),
        table(
            "project",
            &[
                ("id", s, false),
                ("name", s, false),
                ("lowerCaseName", s, false),
                ("issueCountEstimate", n, true),
                ("supportsSearch", b, false),
                ("markURL", s, true),
                ("logoURL", s, true),
            ],
            &["id"],
        ),
        table(
            "issue",
            &[
                ("id", s, false),
                ("shortID", n, true),
                ("title", s, false),
                ("open", b, false),
                ("modified", n, false),
                ("created", n, false),
                ("projectID", s, false),
                ("creatorID", s, false),
                ("assigneeID", s, true),
                ("description", s, false),
                ("visibility", s, false),
            ],
            &["id"],
        ),
        table(
            "comment",
            &[
                ("id", s, false),
                ("issueID", s, false),
                ("created", n, false),
                ("body", s, false),
                ("creatorID", s, false),
            ],
            &["id"],
        ),
        table(
            "label",
            &[
                ("id", s, false),
                ("name", s, false),
                ("projectID", s, false),
            ],
            &["id"],
        ),
        table(
            "issueLabel",
            &[
                ("issueID", s, false),
                ("labelID", s, false),
                ("projectID", s, false),
            ],
            &["labelID", "issueID"],
        ),
        table(
            "viewState",
            &[
                ("issueID", s, false),
                ("userID", s, false),
                ("viewed", n, false),
            ],
            &["userID", "issueID"],
        ),
    ];
    let relationships = [
        relationships(
            "user",
            &[
                (
                    "createdIssues",
                    vec![hop("id", "creatorID", "issue", "many")],
                ),
                (
                    "assignedIssues",
                    vec![hop("id", "assigneeID", "issue", "many")],
                ),
            ],
        ),
        relationships(
            "project",
            &[
                ("issues", vec![hop("id", "projectID", "issue", "many")]),
                ("labels", vec![hop("id", "projectID", "label", "many")]),
            ],
        ),
        relationships(
            "issue",
            &[
                ("project", vec![hop("projectID", "id", "project", "one")]),
                (
                    "issueLabels",
                    vec![hop("id", "issueID", "issueLabel", "many")],
                ),
                (
                    "labels",
                    vec![
                        hop("id", "issueID", "issueLabel", "many"),
                        hop("labelID", "id", "label", "many"),
                    ],
                ),
                ("comments", vec![hop("id", "issueID", "comment", "many")]),
                ("creator", vec![hop("creatorID", "id", "user", "one")]),
                ("assignee", vec![hop("assigneeID", "id", "user", "one")]),
                ("viewState", vec![hop("id", "issueID", "viewState", "many")]),
            ],
        ),
        relationships(
            "comment",
            &[
                ("creator", vec![hop("creatorID", "id", "user", "one")]),
                ("issue", vec![hop("issueID", "id", "issue", "one")]),
            ],
        ),
        relationships(
            "issueLabel",
            &[
                ("issue", vec![hop("issueID", "id", "issue", "one")]),
                ("label", vec![hop("labelID", "id", "label", "one")]),
            ],
        ),
        relationships(
            "label",
            &[("project", vec![hop("projectID", "id", "project", "one")])],
        ),
    ];
    let schema = format!(
        r#"{{"tables": {{{}}}, "relationships": {{{}}}}}"#,
        tables.join(","),
        relationships.join(",")
    );
    Arc::new(Schema::from_json(&json::parse(&schema).unwrap()).unwrap())
}

/// The first page of zbugs' issue list as the app opens it: `issueListV2`
/// (`buildListQuery` in `apps/zbugs/shared/queries.ts`) for the open issues
/// of project Zero, newest change first, signed in but not crew.
pub fn issue_list(schema: &Arc<Schema>) -> AnyQuery {
    AnyQuery::new(schema.clone(), "issue", System::Client)
        .related("viewState", |q| {
            q.where_("userID", SimpleOperator::Eq, USER_ID).one()
        })
        .related("labels", |q| q)
        .related("creator", |q| q)
        .where_exists(
            "project",
            ExistsOptions {
                flip: None,
                scalar: Some(true),
            },
            |q| q.where_("lowerCaseName", SimpleOperator::Eq, "zero"),
        )
        .order_by("modified", Direction::Desc)
        .order_by("id", Direction::Desc)
        .limit(PAGE_SIZE as i64 + 1)
        .where_expr(|eb| eb.and(vec![eb.cmp("open", SimpleOperator::Eq, true)]))
        .where_("visibility", SimpleOperator::Eq, "public")
}

/// The `rowsPatch` puts of the poke, by table, with made-up but zbugs-like
/// contents: nanoid keys, titles, Markdown bodies, and millisecond
/// timestamps.
pub fn rows() -> Vec<String> {
    let mut random = SplitMix64(2064);
    let now = 1_760_000_000_000.0;
    let mut out = Vec::new();
    let mut put = |table: &str, value: Vec<(&str, Value)>| {
        let mut o = Object::new();
        for (k, v) in value {
            o.insert(k, v);
        }
        out.push(format!(
            r#"{{"op":"put","tableName":"{table}","value":{}}}"#,
            json::stringify(&Value::from(o))
        ));
    };
    let s = |s: &str| Value::from(s);

    let project_id = random.nanoid();
    put(
        "project",
        vec![
            ("id", s(&project_id)),
            ("name", s("Zero")),
            ("lowerCaseName", s("zero")),
            ("issueCountEstimate", Value::from(ISSUES as f64)),
            ("supportsSearch", Value::from(true)),
            ("markURL", Value::Null),
            ("logoURL", Value::Null),
        ],
    );

    let mut users = Vec::new();
    for n in 0..USERS {
        let uid = if n == 0 {
            USER_ID.to_owned()
        } else {
            random.nanoid()
        };
        users.push(uid.clone());
        let login = format!("{}{}{n}", random.word(), random.word());
        let name = if n % 3 == 0 {
            Value::Null
        } else {
            s(&capitalize_words(&random.sentence(2, 2)))
        };
        put(
            "user",
            vec![
                ("id", s(&uid)),
                ("login", s(&login)),
                ("name", name),
                (
                    "avatar",
                    s(&format!(
                        "https://avatars.githubusercontent.com/u/{}?v=4",
                        1_000_000 + n * 7919
                    )),
                ),
                ("role", s(if n < 5 { "crew" } else { "user" })),
            ],
        );
    }

    let mut labels = Vec::new();
    for n in 0..LABELS {
        let lid = random.nanoid();
        labels.push(lid.clone());
        let name = format!("{}-{n}", random.word());
        put(
            "label",
            vec![
                ("id", s(&lid)),
                ("name", s(&name)),
                ("projectID", s(&project_id)),
            ],
        );
    }

    let mut issues: Vec<(String, f64)> = Vec::new();
    for n in 0..ISSUES {
        let iid = random.nanoid();
        let created = now - random.below(400 * 86_400) as f64 * 1000.0;
        let modified = created + random.below(30 * 86_400) as f64 * 1000.0;
        issues.push((iid.clone(), created));
        let title = capitalize_first(&random.sentence(4, 12));
        let open = random.below(10) < 6;
        let creator = users[random.below(users.len())].clone();
        let assignee = if random.below(2) == 0 {
            Value::Null
        } else {
            s(&users[random.below(5)])
        };
        let description = random.paragraphs(0, 4);
        let visibility = if random.below(20) == 0 {
            "internal"
        } else {
            "public"
        };
        put(
            "issue",
            vec![
                ("id", s(&iid)),
                ("shortID", Value::from((3000 + n) as f64)),
                ("title", s(&title)),
                ("open", Value::from(open)),
                ("modified", Value::from(modified)),
                ("created", Value::from(created)),
                ("projectID", s(&project_id)),
                ("creatorID", s(&creator)),
                ("assigneeID", assignee),
                ("description", s(&description)),
                ("visibility", s(visibility)),
            ],
        );
    }

    for _ in 0..COMMENTS {
        let (issue, issue_created) = issues[random.below(issues.len())].clone();
        let id = random.nanoid();
        let created = issue_created + random.below(60 * 86_400) as f64 * 1000.0;
        let body = random.paragraphs(1, 3);
        let creator = users[random.below(users.len())].clone();
        put(
            "comment",
            vec![
                ("id", s(&id)),
                ("issueID", s(&issue)),
                ("created", Value::from(created)),
                ("body", s(&body)),
                ("creatorID", s(&creator)),
            ],
        );
    }

    // Distinct (label, issue) pairs.
    let mut pairs = HashSet::new();
    while pairs.len() < ISSUE_LABELS {
        let issue = issues[random.below(issues.len())].0.clone();
        let label = labels[random.below(labels.len())].clone();
        if !pairs.insert(format!("{label}/{issue}")) {
            continue;
        }
        put(
            "issueLabel",
            vec![
                ("issueID", s(&issue)),
                ("labelID", s(&label)),
                ("projectID", s(&project_id)),
            ],
        );
    }
    out
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    chars
        .next()
        .map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
}

fn capitalize_words(s: &str) -> String {
    s.split(' ')
        .map(capitalize_first)
        .collect::<Vec<_>>()
        .join(" ")
}

/// A deterministic generator, so every run syncs the same rows.
struct SplitMix64(u64);

const ALPHABET: &[u8; 64] = b"useandom-26T198340PX75pxJACKVERYMINDBUSHWOLF_GQZbfghjklqvwyzrict";

const WORDS: [&str; 59] = [
    "query",
    "client",
    "sync",
    "issue",
    "label",
    "cache",
    "replica",
    "server",
    "view",
    "row",
    "poke",
    "mutation",
    "schema",
    "index",
    "join",
    "filter",
    "order",
    "limit",
    "permission",
    "auth",
    "token",
    "socket",
    "reconnect",
    "error",
    "crash",
    "slow",
    "fast",
    "scroll",
    "list",
    "page",
    "update",
    "delete",
    "insert",
    "migration",
    "postgres",
    "sqlite",
    "the",
    "a",
    "when",
    "after",
    "before",
    "with",
    "without",
    "should",
    "does",
    "not",
    "work",
    "fails",
    "on",
    "first",
    "load",
    "build",
    "deploy",
    "test",
    "docs",
    "example",
    "support",
    "custom",
    "add",
];

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn nanoid(&mut self) -> String {
        (0..21).map(|_| ALPHABET[self.below(64)] as char).collect()
    }

    fn word(&mut self) -> &'static str {
        WORDS[self.below(WORDS.len())]
    }

    /// `low` to `high` words, inclusive.
    fn sentence(&mut self, low: usize, high: usize) -> String {
        let n = low + self.below(high - low + 1);
        (0..n).map(|_| self.word()).collect::<Vec<_>>().join(" ")
    }

    fn paragraphs(&mut self, low: usize, high: usize) -> String {
        let n = low + self.below(high - low + 1);
        (0..n)
            .map(|_| {
                let sentences = 1 + self.below(4);
                (0..sentences)
                    .map(|_| capitalize_first(&self.sentence(5, 20)) + ".")
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}
