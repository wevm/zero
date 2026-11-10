//! M1.5: connect to a real zero-cache, ask for a named query, and receive
//! its rows. `ZERO_E2E_URL` is the origin of a zero-cache over the fixture
//! database (`packages/zero-conformance/src/e2e/`); without it the test does
//! nothing.

use futures_util::StreamExt;
use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest, http::HeaderValue};
use zero_client::hash::query::hash_of_name_and_args;
use zero_client::json::{self, Object, Value};
use zero_client::protocol::{
    ConnectionUrl, Downstream, InitConnectionBody, PokePartBody, QueriesPatchOp, RowPatchOp,
    UpQueriesPatchOp, UpQueriesPut, poke_chunk, sec_protocols,
};
use zero_client::schema::{NameMapper, Schema};

/// The tables of the zql test schema the fixture's `issues.open` reads
/// (`packages/zql/src/query/test/test-schemas.ts`).
const SCHEMA: &str = r#"{
  "tables": {
    "issue": {"name": "issue", "serverName": "issues", "columns": {
      "id": {"type": "string"}, "title": {"type": "string"},
      "description": {"type": "string"}, "closed": {"type": "boolean"},
      "ownerId": {"type": "string", "optional": true, "serverName": "owner_id"},
      "createdAt": {"type": "number"}}, "primaryKey": ["id"]},
    "user": {"name": "user", "serverName": "users", "columns": {
      "id": {"type": "string"}, "name": {"type": "string"},
      "metadata": {"type": "json", "optional": true}}, "primaryKey": ["id"]},
    "comment": {"name": "comment", "serverName": "comments", "columns": {
      "id": {"type": "string"}, "authorId": {"type": "string"},
      "issueId": {"type": "string", "serverName": "issue_id"},
      "text": {"type": "string"}, "createdAt": {"type": "number"}}, "primaryKey": ["id"]}
  }
}"#;

#[tokio::test]
async fn syncs_the_rows_of_a_query() {
    let Ok(origin) = std::env::var("ZERO_E2E_URL") else {
        eprintln!("ZERO_E2E_URL is not set: skipping the walking skeleton");
        return;
    };
    let schema = Schema::from_json(&json::parse(SCHEMA).unwrap()).unwrap();
    tokio::time::timeout(Duration::from_secs(60), sync(&origin, &schema))
        .await
        .expect("zero-cache synced the query within 60 s");
}

async fn sync(origin: &str, schema: &Schema) {
    // `issues.open` in packages/zero-conformance/src/e2e/app.ts. The server
    // runs its own definition; the client only names it.
    let hash = hash_of_name_and_args("issues.open", &[]);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let client_id = format!("rust-{nanos:x}");
    let init = InitConnectionBody {
        desired_queries_patch: vec![UpQueriesPatchOp::Put(Box::new(UpQueriesPut {
            hash: hash.clone(),
            ttl: Some(60_000.0),
            ast: None,
            name: Some("issues.open".to_owned()),
            args: Some(Vec::new()),
        }))],
        client_schema: Some(schema.client_schema()),
        active_clients: Some(vec![client_id.clone()]),
        ..InitConnectionBody::default()
    };
    let url = ConnectionUrl {
        socket_origin: origin.to_owned(),
        client_group_id: format!("{client_id}-group"),
        client_id: client_id.clone(),
        wsid: "ws1".to_owned(),
        profile_id: format!("p{client_id}"),
        ..ConnectionUrl::default()
    };
    let mut request = url.to_string().into_client_request().unwrap();
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_str(&sec_protocols::encode(Some(&init), None)).unwrap(),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("connected to zero-cache");

    let mut connected = false;
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut rows: BTreeMap<String, Vec<Object>> = BTreeMap::new();
    let mut got_queries: HashSet<String> = HashSet::new();
    let mut apply = |parts: Vec<PokePartBody>, got: &mut HashSet<String>| {
        for part in parts {
            for op in part.got_queries_patch.iter().flatten() {
                if let QueriesPatchOp::Put { hash, .. } = op {
                    got.insert(hash.clone());
                }
            }
            for op in part.rows_patch.into_iter().flatten() {
                if let RowPatchOp::Put { table_name, value } = op {
                    rows.entry(table_name).or_default().push(value);
                }
            }
        }
    };
    while let Some(message) = socket.next().await {
        match message.expect("a message from zero-cache") {
            Message::Binary(bytes) => chunks.push(poke_chunk::payload(&bytes).unwrap().to_vec()),
            Message::Text(text) => match Downstream::from_text(&text).unwrap() {
                Downstream::Connected(_) => connected = true,
                Downstream::PokeStart(_) => chunks.clear(),
                Downstream::PokePart(part) => apply(vec![part], &mut got_queries),
                Downstream::PokeEnd(end) => {
                    if end.cancel != Some(true) {
                        let parts = poke_chunk::decode(chunks.iter().map(Vec::as_slice));
                        apply(parts.unwrap(), &mut got_queries);
                    }
                    if got_queries.contains(&hash) {
                        break;
                    }
                }
                Downstream::Error(e) => panic!("zero-cache error: {e:?}"),
                _ => {}
            },
            Message::Close(frame) => panic!("zero-cache closed the socket: {frame:?}"),
            _ => {}
        }
    }
    socket.close(None).await.ok();

    assert!(connected, "zero-cache sent connected");
    assert!(got_queries.contains(&hash), "zero-cache synced issues.open");
    let to_client = NameMapper::server_to_client(schema);
    let mut client_rows = BTreeMap::new();
    for (table, rows) in &rows {
        for row in rows {
            println!(
                "[e2e] {table} {}",
                json::stringify(&Value::Object(row.clone()))
            );
        }
        let mapped: Vec<Object> = rows
            .iter()
            .map(|r| to_client.row(table, r).unwrap())
            .collect();
        client_rows.insert(table.as_str(), mapped);
    }
    let ids = |table: &str| -> HashSet<String> {
        let rows = client_rows
            .get(table)
            .map(Vec::as_slice)
            .unwrap_or_default();
        rows.iter()
            .map(|r| r.get("id").unwrap().as_str().unwrap().to_owned())
            .collect()
    };
    let set = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<HashSet<_>>();
    // The fixture's open issues, their comments, and their owners.
    assert_eq!(ids("issues"), set(&["i1", "i3", "i4"]));
    assert_eq!(ids("comments"), set(&["c1", "c2", "c3"]));
    assert_eq!(ids("users"), set(&["u1"]));
    let issue = |id: &str| {
        client_rows["issues"]
            .iter()
            .find(|r| r.get("id").and_then(Value::as_str) == Some(id))
    };
    assert_eq!(
        issue("i3").unwrap().get("title"),
        Some(&Value::from("Emoji 😀 in titles"))
    );
    assert_eq!(
        issue("i1").unwrap().get("ownerId"),
        Some(&Value::from("u1"))
    );
    assert_eq!(
        client_rows["users"][0].get("metadata").cloned(),
        Some(json::parse(r#"{"registrar": "github", "email": "alice@example.com"}"#).unwrap())
    );
}
