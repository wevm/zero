//! The inspector (`packages/zero-client/src/client/inspector`): the
//! client's queries and metrics, what the server knows of them, and the
//! server's analysis of a query, for debugging and tooling.
//!
//! Its RPCs go over the sync socket as `inspect` messages
//! (`packages/zero-protocol/src/inspect-up.ts` and `inspect-down.ts`).

use super::engine::Engine;
use super::planner_debug;
use super::{ClientErrorKind, ConnectionError, Deferred, Error};
use crate::ast::{Ast, ast_to_zql};
use crate::decode::JsonCodable;
use crate::json::{Object, Value};
use crate::keys::ENTITIES_PREFIX;
use crate::map::OrderedMap;
use crate::query::ZeroQuery;
use crate::schema::{NameMapper, Schema};
use crate::tdigest::TDigest;
use crate::ttl::Ttl;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

/// The client's and the server's query metrics (`Metrics` in
/// `lazy-inspector.ts`), each a digest of times in ms.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metrics {
    /// `query-materialization-client`: building and hydrating a view.
    pub query_materialization_client: TDigest,
    /// `query-materialization-end-to-end`: from materializing a query
    /// until the server has sent its rows.
    pub query_materialization_end_to_end: TDigest,
    /// `query-update-client`: a change through a query's pipeline. Only one
    /// change in 32 is timed, and counts 32 times.
    pub query_update_client: TDigest,
    /// `query-materialization-server`: the server hydrating a query.
    pub query_materialization_server: TDigest,
    /// `query-update-server`: a change through the server's pipeline.
    pub query_update_server: TDigest,
}

impl Metrics {
    /// The digests by JS name, as their JSON forms.
    pub fn to_json(&self) -> Value {
        let mut o = Object::new();
        o.insert(
            "query-materialization-client",
            self.query_materialization_client.to_json(),
        );
        o.insert(
            "query-materialization-end-to-end",
            self.query_materialization_end_to_end.to_json(),
        );
        o.insert("query-update-client", self.query_update_client.to_json());
        o.insert(
            "query-materialization-server",
            self.query_materialization_server.to_json(),
        );
        o.insert("query-update-server", self.query_update_server.to_json());
        Value::Object(o)
    }
}

/// One query's client metrics (`QueryClientMetrics`): the last
/// materialization's times, and its updates.
#[derive(Clone, Debug, Default)]
pub(crate) struct QueryClientMetrics {
    pub(crate) query_materialization_client: Option<f64>,
    pub(crate) query_materialization_end_to_end: Option<f64>,
    pub(crate) query_update_client: TDigest,
}

/// What [`Inspector::analyze_query`] asks the server for besides the
/// basics (`AnalyzeQueryOptions`). Options left `None` are not sent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AnalyzeQueryOptions {
    /// The rows the query read from each source.
    pub vended_rows: Option<bool>,
    /// The rows the query synced, by table.
    pub synced_rows: Option<bool>,
    /// The planner's debug events, for
    /// [`Inspector::format_planner_events`].
    pub join_plans: Option<bool>,
}

impl AnalyzeQueryOptions {
    /// The options as sent; `None` when none is set.
    fn to_json(&self) -> Option<Value> {
        let mut o = Object::new();
        for (key, v) in [
            ("vendedRows", self.vended_rows),
            ("syncedRows", self.synced_rows),
            ("joinPlans", self.join_plans),
        ] {
            if let Some(v) = v {
                o.insert(key, Value::Bool(v));
            }
        }
        (!o.is_empty()).then_some(Value::Object(o))
    }
}

/// One of the planner's debug events (`PlanDebugEventJSON` in
/// `packages/zero-protocol/src/analyze-query-result.ts`), as JSON: its
/// `type` says which.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanDebugEvent(pub Value);

/// Rows by query (or table).
pub type RowsByQuery = OrderedMap<Vec<Object>>;
/// Row counts by source, then by query.
pub type RowCountsBySource = OrderedMap<OrderedMap<f64>>;
/// Rows by source, then by query.
pub type RowsBySource = OrderedMap<RowsByQuery>;

/// The server's analysis of a query (`AnalyzeQueryResult` in
/// `packages/zero-protocol/src/analyze-query-result.ts`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AnalyzeQueryResult {
    /// What the server warns about the query.
    pub warnings: Vec<String>,
    /// The rows synced, by table, when asked for.
    pub synced_rows: Option<RowsByQuery>,
    /// How many rows were synced.
    pub synced_row_count: f64,
    /// When the analysis started, in ms.
    pub start: f64,
    /// When it ended, in ms.
    pub end: f64,
    /// How long it took, in ms.
    pub elapsed: Option<f64>,
    /// The query after the permissions applied, as ZQL.
    pub after_permissions: Option<String>,
    /// The rows each source vended, counted.
    pub vended_row_counts: Option<RowCountsBySource>,
    /// The rows each source vended, when asked for.
    pub vended_rows: Option<RowsBySource>,
    /// SQLite's query plans, by query.
    pub sqlite_plans: Option<OrderedMap<Vec<String>>>,
    /// The rows read, by source and query.
    pub read_rows: Option<RowsBySource>,
    /// The rows read, counted by source and query.
    pub read_row_counts_by_query: Option<RowCountsBySource>,
    /// How many rows were read.
    pub read_row_count: Option<f64>,
    /// The database scans, counted by source and query.
    pub db_scans_by_query: Option<RowCountsBySource>,
    /// The planner's debug events, when asked for.
    pub join_plans: Option<Vec<PlanDebugEvent>>,
}

fn record<T>(v: Option<&Value>, f: impl Fn(&Value) -> T) -> Option<OrderedMap<T>> {
    let o = v?.as_object()?;
    Some(o.iter().map(|(k, v)| (k.clone(), f(v))).collect())
}

fn rows(v: &Value) -> Vec<Object> {
    v.as_array()
        .map(|a| a.iter().filter_map(|r| r.as_object().cloned()).collect())
        .unwrap_or_default()
}

fn number(v: &Value) -> f64 {
    v.as_f64().unwrap_or(f64::NAN)
}

fn record_json<T>(r: &OrderedMap<T>, f: impl Fn(&T) -> Value) -> Value {
    Value::Object(r.iter().map(|(k, v)| (k.clone(), f(v))).collect())
}

fn rows_json(rows: &[Object]) -> Value {
    Value::Array(rows.iter().cloned().map(Value::Object).collect())
}

impl AnalyzeQueryResult {
    /// The result of the JSON the server sent, already checked against
    /// `analyzeQueryResultSchema`.
    pub fn from_json(v: &Value) -> Self {
        let get = |k: &str| v.get(k);
        let counts = |k: &str| record(get(k), |v| record(Some(v), number).unwrap_or_default());
        let by_source = |k: &str| record(get(k), |v| record(Some(v), rows).unwrap_or_default());
        AnalyzeQueryResult {
            warnings: get("warnings")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|w| w.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            synced_rows: record(get("syncedRows"), rows),
            synced_row_count: get("syncedRowCount").map_or(f64::NAN, number),
            start: get("start").map_or(f64::NAN, number),
            end: get("end").map_or(f64::NAN, number),
            elapsed: get("elapsed").and_then(Value::as_f64),
            after_permissions: get("afterPermissions")
                .and_then(Value::as_str)
                .map(str::to_owned),
            vended_row_counts: counts("vendedRowCounts"),
            vended_rows: by_source("vendedRows"),
            sqlite_plans: record(get("sqlitePlans"), |v| {
                v.as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|s| s.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default()
            }),
            read_rows: by_source("readRows"),
            read_row_counts_by_query: counts("readRowCountsByQuery"),
            read_row_count: get("readRowCount").and_then(Value::as_f64),
            db_scans_by_query: counts("dbScansByQuery"),
            join_plans: get("joinPlans")
                .and_then(Value::as_array)
                .map(|a| a.iter().cloned().map(PlanDebugEvent).collect()),
        }
    }

    /// The result as JSON, as the server sent it.
    pub fn to_json(&self) -> Value {
        let counts =
            |c: &RowCountsBySource| record_json(c, |q| record_json(q, |n| Value::Number(*n)));
        let by_source = |r: &RowsBySource| record_json(r, |q| record_json(q, |r| rows_json(r)));
        let mut o = Object::new();
        o.insert(
            "warnings",
            Value::Array(self.warnings.iter().cloned().map(Value::String).collect()),
        );
        if let Some(r) = &self.synced_rows {
            o.insert("syncedRows", record_json(r, |r| rows_json(r)));
        }
        o.insert("syncedRowCount", Value::Number(self.synced_row_count));
        o.insert("start", Value::Number(self.start));
        o.insert("end", Value::Number(self.end));
        if let Some(e) = self.elapsed {
            o.insert("elapsed", Value::Number(e));
        }
        if let Some(a) = &self.after_permissions {
            o.insert("afterPermissions", Value::String(a.clone()));
        }
        if let Some(c) = &self.vended_row_counts {
            o.insert("vendedRowCounts", counts(c));
        }
        if let Some(r) = &self.vended_rows {
            o.insert("vendedRows", by_source(r));
        }
        if let Some(p) = &self.sqlite_plans {
            o.insert(
                "sqlitePlans",
                record_json(p, |p| {
                    Value::Array(p.iter().cloned().map(Value::String).collect())
                }),
            );
        }
        if let Some(r) = &self.read_rows {
            o.insert("readRows", by_source(r));
        }
        if let Some(c) = &self.read_row_counts_by_query {
            o.insert("readRowCountsByQuery", counts(c));
        }
        if let Some(n) = self.read_row_count {
            o.insert("readRowCount", Value::Number(n));
        }
        if let Some(c) = &self.db_scans_by_query {
            o.insert("dbScansByQuery", counts(c));
        }
        if let Some(p) = &self.join_plans {
            o.insert(
                "joinPlans",
                Value::Array(p.iter().map(|e| e.0.clone()).collect()),
            );
        }
        Value::Object(o)
    }
}

/// A query as the server and the client know it (`Query` in
/// `inspector/query.ts`): one row of the server's `queries` answer, with
/// the client's metrics merged in.
#[derive(Clone, Debug, PartialEq)]
pub struct InspectorQuery {
    /// The named query's name; `None` for an unnamed one.
    pub name: Option<String>,
    /// The named query's arguments; `None` for an unnamed one.
    pub args: Option<Vec<Value>>,
    /// Whether the server has synced the query's rows.
    pub got: bool,
    /// The query's TTL, normalized.
    pub ttl: Ttl,
    /// When the query became inactive, in ms since 1970; `None` while
    /// active.
    pub inactivated_at: Option<f64>,
    /// How many rows the query synced.
    pub row_count: f64,
    /// Whether the server deleted the query.
    pub deleted: bool,
    /// The query's ID, its hash.
    pub id: String,
    /// The client that wants it.
    pub client_id: String,
    /// The client's and the server's metrics of it.
    pub metrics: Metrics,
    /// The query as this client built it, as ZQL; `None` when this client
    /// doesn't know it.
    pub client_zql: Option<String>,
    /// The query the server runs, as ZQL.
    pub server_zql: Option<String>,
    /// How long the client took to hydrate it, in ms.
    pub hydrate_client: Option<f64>,
    /// How long the server took to hydrate it, in ms.
    pub hydrate_server: Option<f64>,
    /// How long it took from materializing to the server's rows, in ms.
    pub hydrate_total: Option<f64>,
    /// The median client update, in ms.
    pub update_client_p50: Option<f64>,
    /// The 95th percentile client update, in ms.
    pub update_client_p95: Option<f64>,
    /// The median server update, in ms.
    pub update_server_p50: Option<f64>,
    /// The 95th percentile server update, in ms.
    pub update_server_p95: Option<f64>,
}

/// `TimeClip` of `new Date(ms)`: the integer part, NaN past ±8.64e15.
fn time_clip(ms: f64) -> f64 {
    if !ms.is_finite() || ms.abs() > 8.64e15 {
        return f64::NAN;
    }
    ms.trunc() + 0.0
}

fn percentile(d: &TDigest, q: f64) -> Option<f64> {
    let n = d.quantile(q);
    (!n.is_nan()).then_some(n)
}

fn zql(ast: &Ast) -> String {
    format!("{}{}", ast.table, ast_to_zql(ast, false))
}

impl InspectorQuery {
    /// The query of a row of the server's `queries` answer.
    fn new(row: &Value, engine: &Engine) -> Self {
        let get = |k: &str| row.get(k);
        let str_of = |k: &str| {
            get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let id = str_of("queryID");
        let server_ast = get("ast").and_then(|a| Ast::from_json(a).ok());
        let client_metrics = engine.query_manager.query_metrics(&id);
        let server = get("metrics").filter(|m| !m.is_null());
        let hydrate_server = server
            .and_then(|m| m.get("query-hydration-server-ms"))
            .and_then(Value::as_f64);
        let cm = client_metrics.clone().unwrap_or_default();
        let mut metrics = Metrics {
            query_update_client: cm.query_update_client.clone(),
            ..Metrics::default()
        };
        if let Some(v) = cm.query_materialization_client {
            metrics.query_materialization_client.add(v, 1.0);
        }
        if let Some(v) = cm.query_materialization_end_to_end {
            metrics.query_materialization_end_to_end.add(v, 1.0);
        }
        if let Some(server) = server {
            if let Some(ms) = hydrate_server {
                metrics.query_materialization_server.add(ms, 1.0);
            }
            if let Some(d) = server
                .get("query-update-server")
                .and_then(|d| TDigest::from_json_value(d).ok())
            {
                metrics.query_update_server = d;
            }
        }
        InspectorQuery {
            name: get("name").and_then(Value::as_str).map(str::to_owned),
            args: get("args").and_then(Value::as_array).cloned(),
            got: get("got").and_then(Value::as_bool).unwrap_or_default(),
            ttl: Ttl::Milliseconds(get("ttl").and_then(Value::as_f64).unwrap_or_default())
                .normalized(),
            inactivated_at: get("inactivatedAt").and_then(Value::as_f64).map(time_clip),
            row_count: get("rowCount").and_then(Value::as_f64).unwrap_or_default(),
            deleted: get("deleted").and_then(Value::as_bool).unwrap_or_default(),
            client_id: str_of("clientID"),
            client_zql: engine.query_manager.query_ast(&id).as_ref().map(zql),
            server_zql: server_ast.as_ref().map(zql),
            hydrate_client: client_metrics
                .as_ref()
                .and_then(|m| m.query_materialization_client),
            hydrate_server,
            hydrate_total: client_metrics
                .as_ref()
                .and_then(|m| m.query_materialization_end_to_end),
            update_client_p50: percentile(&metrics.query_update_client, 0.5),
            update_client_p95: percentile(&metrics.query_update_client, 0.95),
            update_server_p50: percentile(&metrics.query_update_server, 0.5),
            update_server_p95: percentile(&metrics.query_update_server, 0.95),
            metrics,
            id,
        }
    }
}

/// What the inspector's handles share: the client, and the admin password
/// zero-cache last accepted (`adminPassword`), which is presented again
/// when a new connection isn't authenticated yet.
pub(crate) struct InspectorHost {
    pub(crate) engine: Box<dyn Fn() -> Option<Rc<Engine>>>,
    pub(crate) schema: Arc<Schema>,
    pub(crate) admin_password: RefCell<Option<String>>,
}

/// The error of an RPC the server refused for want of authentication.
/// JS prompts for the password there.
fn authentication_required() -> Error {
    Error::Message(
        "Authentication failed: zero-cache requires the inspector to authenticate. \
         Call `authenticate(password)` with its admin password first."
            .into(),
    )
}

fn closed() -> Error {
    ConnectionError::client(
        ClientErrorKind::ClientClosed,
        "Zero instance closed by user",
    )
    .into()
}

enum Answer {
    Done(Result<Value, Error>),
    Unauthenticated,
}

/// What an answer means for an RPC expecting `expect` (`rpcNoAuthTry`).
fn classify(answer: &Result<Object, Error>, expect: &str) -> Answer {
    let body = match answer {
        Ok(b) => b,
        Err(e) => return Answer::Done(Err(e.clone())),
    };
    let op = body.get("op").and_then(Value::as_str).unwrap_or_default();
    let value = body.get("value").cloned().unwrap_or_default();
    if op == "error" {
        let message = value.as_str().unwrap_or_default().to_owned();
        return Answer::Done(Err(Error::Message(message)));
    }
    if op == expect {
        return Answer::Done(Ok(value));
    }
    if op == "authenticated" && value == Value::Bool(false) {
        return Answer::Unauthenticated;
    }
    Answer::Done(Err(Error::Message(format!(
        "Expected an inspector \"{expect}\" answer, got \"{op}\""
    ))))
}

fn op(name: &str) -> Object {
    let mut o = Object::new();
    o.insert("op", Value::from(name));
    o
}

impl InspectorHost {
    fn engine(&self) -> Option<Rc<Engine>> {
        let e = (self.engine)()?;
        e.query_manager.keep_evicted();
        Some(e)
    }

    /// Sends `body` with a new ID; settles with the server's answer.
    fn send(&self, body: Object) -> Deferred<Object> {
        let out = Deferred::new();
        match self.engine() {
            None => out.reject(closed()),
            Some(e) => {
                let o = out.clone();
                e.event_loop
                    .call(|| e.inspect(body, Box::new(move |r| o.settle(r))));
            }
        }
        out
    }

    /// `authenticateSocket`.
    fn authenticate_once(&self, password: &str) -> Deferred<bool> {
        let mut body = op("authenticate");
        body.insert("value", Value::from(password));
        let out = Deferred::new();
        let o = out.clone();
        self.send(body)
            .on_settle(move |r| match classify(r, "authenticated") {
                Answer::Done(r) => o.settle(r.map(|v| v == Value::Bool(true))),
                Answer::Unauthenticated => o.resolve(false),
            });
        out
    }

    /// `rpc`: sends `body`, and when the connection isn't authenticated,
    /// presents the password accepted last and tries again.
    fn rpc(self: &Rc<Self>, body: Object, expect: &'static str) -> Deferred<Value> {
        let out = Deferred::new();
        let (host, o) = (self.clone(), out.clone());
        self.send(body.clone())
            .on_settle(move |r| match classify(r, expect) {
                Answer::Done(r) => o.settle(r),
                Answer::Unauthenticated => host.authenticate_and_retry(body, expect, o),
            });
        out
    }

    fn authenticate_and_retry(
        self: &Rc<Self>,
        body: Object,
        expect: &'static str,
        out: Deferred<Value>,
    ) {
        let Some(password) = self.admin_password.borrow().clone() else {
            return out.reject(authentication_required());
        };
        let host = self.clone();
        self.authenticate_once(&password)
            .on_settle(move |r| match r {
                Err(e) => out.reject(e.clone()),
                Ok(false) => {
                    *host.admin_password.borrow_mut() = None;
                    out.reject(authentication_required());
                }
                Ok(true) => host
                    .send(body)
                    .on_settle(move |r| match classify(r, expect) {
                        Answer::Done(r) => out.settle(r),
                        Answer::Unauthenticated => out.reject(authentication_required()),
                    }),
            });
    }

    /// The server's `queries` answer, as queries, slowest server hydration
    /// first.
    fn queries(self: &Rc<Self>, client_id: Option<&str>) -> Deferred<Vec<InspectorQuery>> {
        let mut body = op("queries");
        if let Some(id) = client_id {
            body.insert("clientID", Value::from(id));
        }
        let out = Deferred::new();
        let (host, o) = (self.clone(), out.clone());
        self.rpc(body, "queries").on_settle(move |r| {
            let rows = match r {
                Ok(v) => v.as_array().cloned().unwrap_or_default(),
                Err(e) => return o.reject(e.clone()),
            };
            let Some(engine) = host.engine() else {
                return o.reject(closed());
            };
            let mut queries: Vec<InspectorQuery> = rows
                .iter()
                .map(|r| InspectorQuery::new(r, &engine))
                .collect();
            queries.sort_by(|a, b| {
                let (a, b) = (
                    a.hydrate_server.unwrap_or(0.0),
                    b.hydrate_server.unwrap_or(0.0),
                );
                b.partial_cmp(&a).unwrap_or(std::cmp::Ordering::Equal)
            });
            o.resolve(queries);
        });
        out
    }

    /// The clients in the local store whose group `group` (any for
    /// `None`) is.
    fn clients(self: &Rc<Self>, group: Option<&str>) -> Deferred<Vec<InspectorClient>> {
        let out = Deferred::new();
        match self.engine() {
            None => out.reject(closed()),
            Some(e) => out.resolve(
                e.database
                    .clients()
                    .into_iter()
                    .filter(|c| group.is_none_or(|g| c.client_group_id == g))
                    .map(|c| InspectorClient {
                        host: self.clone(),
                        id: c.id,
                        client_group_id: c.client_group_id,
                    })
                    .collect(),
            ),
        }
        out
    }

    /// The clients of `clients` the server has queries of, in the order
    /// it answers (`clientsWithQueries`).
    fn clients_with_queries(
        self: &Rc<Self>,
        clients: Deferred<Vec<InspectorClient>>,
    ) -> Deferred<Vec<InspectorClient>> {
        let out = Deferred::new();
        let o = out.clone();
        clients.on_settle(move |r| {
            let clients = match r {
                Ok(c) => c.clone(),
                Err(e) => return o.reject(e.clone()),
            };
            if clients.is_empty() {
                return o.resolve(Vec::new());
            }
            let found: Rc<RefCell<Vec<InspectorClient>>> = Rc::default();
            let left = Rc::new(Cell::new(clients.len()));
            for client in clients {
                let (o, found, left) = (o.clone(), found.clone(), left.clone());
                let queries = client.queries();
                queries.on_settle(move |r| match r {
                    Err(e) => o.reject(e.clone()),
                    Ok(q) => {
                        if !q.is_empty() {
                            found.borrow_mut().push(client);
                        }
                        left.set(left.get() - 1);
                        if left.get() == 0 {
                            o.resolve(found.borrow().clone());
                        }
                    }
                });
            }
        });
        out
    }

    /// The entries of the store of `client_id`'s group with keys starting
    /// with `prefix`, in key order.
    fn entries(&self, client_id: &str, prefix: &str) -> Result<Vec<(String, Value)>, Error> {
        let e = self.engine().ok_or_else(closed)?;
        let client = e
            .database
            .clients()
            .into_iter()
            .find(|c| c.id == client_id)
            .ok_or_else(|| Error::Message(format!("Client not found: {client_id}")))?;
        let collect = |m: &super::replica_store::KvMap| {
            m.scan(prefix)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>()
        };
        // The client's own group is what the store holds now, as JS
        // persists it before reading.
        if client.client_group_id == e.client_group_id {
            return Ok(collect(&e.store.borrow().main));
        }
        let group = e
            .database
            .group_entries(&client.client_group_id)
            .ok_or_else(|| {
                Error::Message(format!(
                    "Client group not found: {}",
                    client.client_group_id
                ))
            })?;
        Ok(collect(&group))
    }
}

fn settled<T: Clone + 'static>(r: Result<T, Error>) -> Deferred<T> {
    let d = Deferred::new();
    d.settle(r);
    d
}

/// The inspector of a client (`Inspector` in `inspector/inspector.ts`),
/// from [`Zero::inspector`](super::Zero::inspector).
///
/// zero-cache may require its admin password first: an RPC it refuses
/// fails with an error saying so (JS prompts for the password instead).
/// Once [`authenticate`](Self::authenticate) succeeds, the password is
/// presented again when a new connection isn't authenticated yet.
///
/// An RPC waits for the connection. It fails when the connection drops
/// before the answer, and when the client closes.
pub struct Inspector<C = ()> {
    pub(crate) host: Rc<InspectorHost>,
    pub(crate) context: Rc<C>,
    pub(crate) client_id: String,
    pub(crate) client_group_id: String,
}

impl<C> Clone for Inspector<C> {
    fn clone(&self) -> Self {
        Inspector {
            host: self.host.clone(),
            context: self.context.clone(),
            client_id: self.client_id.clone(),
            client_group_id: self.client_group_id.clone(),
        }
    }
}

impl<C> Inspector<C> {
    /// This client.
    pub fn client(&self) -> InspectorClient {
        InspectorClient {
            host: self.host.clone(),
            id: self.client_id.clone(),
            client_group_id: self.client_group_id.clone(),
        }
    }

    /// This client's group.
    pub fn client_group(&self) -> InspectorClientGroup {
        self.client().client_group()
    }

    /// The client's query metrics merged with the server's
    /// (`inspectorMetrics`).
    pub fn metrics(&self) -> Deferred<Metrics> {
        let out = Deferred::new();
        let (host, o) = (self.host.clone(), out.clone());
        self.host.rpc(op("metrics"), "metrics").on_settle(move |r| {
            let server = match r {
                Ok(v) => v.clone(),
                Err(e) => return o.reject(e.clone()),
            };
            let Some(engine) = host.engine() else {
                return o.reject(closed());
            };
            let digest = |k: &str| {
                server
                    .get(k)
                    .and_then(|d| TDigest::from_json_value(d).ok())
                    .unwrap_or_default()
            };
            let mut metrics = engine.query_manager.metrics();
            metrics.query_materialization_server = digest("query-materialization-server");
            metrics.query_update_server = digest("query-update-server");
            o.resolve(metrics);
        });
        out
    }

    /// Every client in the local store.
    pub fn clients(&self) -> Deferred<Vec<InspectorClient>> {
        self.host.clients(None)
    }

    /// The clients in the local store the server has queries of: asks the
    /// server for each client's queries.
    pub fn clients_with_queries(&self) -> Deferred<Vec<InspectorClient>> {
        self.host.clients_with_queries(self.host.clients(None))
    }

    /// zero-cache's version.
    pub fn server_version(&self) -> Deferred<String> {
        let out = Deferred::new();
        let o = out.clone();
        self.host.rpc(op("version"), "version").on_settle(move |r| {
            o.settle(r.clone().map(|v| v.as_str().unwrap_or_default().to_owned()))
        });
        out
    }

    fn analyze(
        &self,
        mut body: Object,
        options: &AnalyzeQueryOptions,
    ) -> Deferred<AnalyzeQueryResult> {
        if let Some(o) = options.to_json() {
            body.insert("options", o);
        }
        let out = Deferred::new();
        let o = out.clone();
        self.host.rpc(body, "analyze-query").on_settle(move |r| {
            o.settle(
                r.as_ref()
                    .map(AnalyzeQueryResult::from_json)
                    .map_err(Clone::clone),
            )
        });
        out
    }

    /// Asks the server to analyze `query`: by name and arguments for a
    /// named query, and by its AST, with the server's names, otherwise.
    pub fn analyze_query(
        &self,
        query: &impl ZeroQuery<C>,
        options: AnalyzeQueryOptions,
    ) -> Deferred<AnalyzeQueryResult> {
        let query = query.any_query(&self.context);
        let mut body = op("analyze-query");
        match query.custom_query_id() {
            Some(id) => {
                body.insert("name", Value::from(id.name.as_str()));
                body.insert("args", Value::Array(id.args.clone()));
            }
            None => match NameMapper::client_to_server(&self.host.schema).map(query.ast()) {
                Ok(ast) => {
                    body.insert("ast", ast.to_json());
                }
                Err(e) => return settled(Err(Error::Message(e.to_string()))),
            },
        }
        self.analyze(body, &options)
    }

    /// Asks the server to analyze `ast`, which is sent as is: it must
    /// have the server's names.
    pub fn analyze_server_ast(
        &self,
        ast: &Ast,
        options: AnalyzeQueryOptions,
    ) -> Deferred<AnalyzeQueryResult> {
        let mut body = op("analyze-query");
        body.insert("ast", ast.to_json());
        self.analyze(body, &options)
    }

    /// Asks the server to analyze the named query `name` with `args`,
    /// which its query handler resolves.
    pub fn analyze_named_query(
        &self,
        name: &str,
        args: Vec<Value>,
        options: AnalyzeQueryOptions,
    ) -> Deferred<AnalyzeQueryResult> {
        let mut body = op("analyze-query");
        body.insert("name", Value::from(name));
        body.insert("args", Value::Array(args));
        self.analyze(body, &options)
    }

    /// Authenticates with zero-cache's admin password: true when it
    /// accepts it (or runs in a development mode that doesn't check).
    /// zero-cache authenticates each connection; an accepted password is
    /// presented again on a new one.
    pub fn authenticate(&self, password: &str) -> Deferred<bool> {
        let out = Deferred::new();
        let (host, o, password) = (self.host.clone(), out.clone(), password.to_owned());
        self.host.authenticate_once(&password).on_settle(move |r| {
            if matches!(r, Ok(true)) {
                *host.admin_password.borrow_mut() = Some(password);
            }
            o.settle(r.clone());
        });
        out
    }

    /// The planner's debug events (`AnalyzeQueryResult::join_plans`) as
    /// text: each attempt's costs, then the plan chosen.
    pub fn format_planner_events(&self, events: &[PlanDebugEvent]) -> String {
        planner_debug::format_planner_events(events)
    }
}

/// A client in the local store (`Client` in `inspector/client.ts`).
#[derive(Clone)]
pub struct InspectorClient {
    host: Rc<InspectorHost>,
    id: String,
    client_group_id: String,
}

impl std::fmt::Debug for InspectorClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InspectorClient")
            .field("id", &self.id)
            .field("client_group_id", &self.client_group_id)
            .finish()
    }
}

impl InspectorClient {
    /// The client's ID.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The client's group.
    pub fn client_group(&self) -> InspectorClientGroup {
        InspectorClientGroup {
            host: self.host.clone(),
            id: self.client_group_id.clone(),
        }
    }

    /// The server's queries of this client.
    pub fn queries(&self) -> Deferred<Vec<InspectorQuery>> {
        self.host.queries(Some(&self.id))
    }

    /// Every entry of the client's local store, in key order: its rows,
    /// desired and got queries, and the rest.
    pub fn map(&self) -> Deferred<OrderedMap<Value>> {
        settled(
            self.host
                .entries(&self.id, "")
                .map(|e| e.into_iter().collect()),
        )
    }

    /// The rows of table `table` in the client's local store, in key
    /// order.
    pub fn rows(&self, table: &str) -> Deferred<Vec<Object>> {
        let prefix = format!("{ENTITIES_PREFIX}{table}/");
        settled(self.host.entries(&self.id, &prefix).map(|e| {
            e.into_iter()
                .filter_map(|(_, v)| match v {
                    Value::Object(o) => Some(o),
                    _ => None,
                })
                .collect()
        }))
    }
}

/// A client group in the local store (`ClientGroup` in
/// `inspector/client-group.ts`).
#[derive(Clone)]
pub struct InspectorClientGroup {
    host: Rc<InspectorHost>,
    id: String,
}

impl std::fmt::Debug for InspectorClientGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InspectorClientGroup")
            .field("id", &self.id)
            .finish()
    }
}

impl InspectorClientGroup {
    /// The group's ID.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The group's clients in the local store.
    pub fn clients(&self) -> Deferred<Vec<InspectorClient>> {
        self.host.clients(Some(&self.id))
    }

    /// The group's clients the server has queries of.
    pub fn clients_with_queries(&self) -> Deferred<Vec<InspectorClient>> {
        self.host
            .clients_with_queries(self.host.clients(Some(&self.id)))
    }

    /// The server's queries of the group's clients.
    pub fn queries(&self) -> Deferred<Vec<InspectorQuery>> {
        self.host.queries(None)
    }
}
