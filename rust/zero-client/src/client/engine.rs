//! The client engine (`Zero` in `packages/zero-client/src/client/zero.ts`,
//! with the parts of Replicache it uses): the run loop that keeps a
//! connection, the replica and its pending mutations, and the query and
//! mutation bookkeeping. Everything runs synchronously on one thread; see
//! `event_loop`.

use super::connection_manager::{ConnectionManager, State};
use super::database::{AggregateWrites, ReplicaDatabase, StorageOp, StoredMutation, base36};
use super::event_loop::{EventLoop, LoopScheduler};
use super::ivm_branch::IvmBranch;
use super::log::Log;
use super::mutation_tracker::MutationTracker;
use super::poke_handler::{MergedPoke, PokeHandler};
use super::query_context::QueryContext;
use super::query_manager::QueryManager;
use super::replica_store::{
    KvDiff, KvMap, KvPatchOp, KvWrite, PendingMutation, ReplicaStore, Snapshot, apply_patch,
    compare_cookies, diff_all, diff_keys, with_prefix,
};
use super::socket::{SocketConnection, SocketEvents, SocketFactory, SocketMessage};
use super::storage::{LoadError, StorageFailure, ZeroStorage};
use super::transaction::{Reason, Transaction};
use super::{
    ApplicationError, ClientErrorKind, ConnectionError, ConnectionStatus, ConnectionTransition,
    Deferred, Error, LogLevel, LogSink, Scheduler, StderrLogSink, Timer,
};
use crate::decode::{DecodeError, JsonCodable};
use crate::hash::xxhash32;
use crate::ivm::{Row, capture};
use crate::json::{self, Object, Value, number};
use crate::keys::{self, AGGREGATES_PREFIX, GOT_QUERIES_PREFIX, MUTATIONS_PREFIX};
use crate::map::OrderedMap;
use crate::protocol::{
    ChangeDesiredQueriesBody, ConnectedBody, ConnectionUrl, DeleteClientsBody, Downstream,
    ErrorBody, ErrorKind, InitConnectionBody, Mutation, PushBody, PushResponseBody,
    UpQueriesPatchOp, UpdateAuthBody, Upstream, poke_chunk, sec_protocols,
};
use crate::schema::Schema;
use crate::strings;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{BuildHasher, Hasher};
use std::rc::{Rc, Weak};
use std::sync::Arc;

/// `RUN_LOOP_INTERVAL_MS`: the backoff before retrying a failed connection.
pub(crate) const RUN_LOOP_INTERVAL_MS: f64 = 5_000.0;
/// `FAST_RECONNECT_INITIAL_MS`: the longest wait before the first retry
/// after a transport failure. Each further consecutive failure doubles it,
/// up to `RUN_LOOP_INTERVAL_MS`.
const FAST_RECONNECT_INITIAL_MS: f64 = 500.0;
/// `STABLE_CONNECTION_MS`: how long a connection must stay up before a
/// later failure starts the fast schedule from the beginning again.
const STABLE_CONNECTION_MS: f64 = 5_000.0;
/// `CONNECT_TIMEOUT_MS`: each phase of a connection attempt gets this long.
const CONNECT_TIMEOUT_MS: f64 = 10_000.0;
/// `HEARTBEAT_INTERVAL`.
const HEARTBEAT_INTERVAL_MS: f64 = 60_000.0;
/// `GC_INTERVAL`, of clients and of client groups.
const GC_INTERVAL_MS: f64 = 5.0 * 60_000.0;
/// `CLIENT_MAX_INACTIVE_TIME`.
const CLIENT_MAX_INACTIVE_MS: f64 = 24.0 * 60.0 * 60_000.0;
/// `INITIAL_COLLECT_IDB_DELAY`.
const INITIAL_COLLECT_DATABASES_DELAY_MS: f64 = 5.0 * 60_000.0;
/// `COLLECT_IDB_INTERVAL`.
const COLLECT_DATABASES_INTERVAL_MS: f64 = 12.0 * 60.0 * 60_000.0;
/// `PERSIST_IDLE_TIMEOUT_MS` and `PERSIST_THROTTLE_MS`.
const PERSIST_IDLE_TIMEOUT_MS: f64 = 1000.0;
const PERSIST_THROTTLE_MS: f64 = 500.0;

/// `reconnectBackoffMs`: the wait before retrying after `failed_attempts`
/// consecutive transport failures (1 for the first). The ceiling starts at
/// `FAST_RECONNECT_INITIAL_MS` and doubles with each failure, and the wait
/// is between half the ceiling and the ceiling, so that clients that lost
/// the same server don't all retry at once. Once the ceiling reaches
/// `max_ms`, the wait is `max_ms`.
pub(crate) fn reconnect_backoff_ms(failed_attempts: u32, max_ms: f64, random: f64) -> f64 {
    let ceiling = FAST_RECONNECT_INITIAL_MS * 2f64.powi(failed_attempts as i32 - 1);
    if ceiling >= max_ms {
        return max_ms;
    }
    (ceiling / 2.0 + random * ceiling / 2.0).round()
}

thread_local! {
    static RNG: Cell<u64> = Cell::new({
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        h.finish() | 1
    });
}

/// A random 64-bit number (xorshift64*, seeded per thread from the OS).
fn random_u64() -> u64 {
    RNG.with(|r| {
        let mut x = r.get();
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        r.set(x);
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    })
}

/// `Math.random()`.
fn random_f64() -> f64 {
    (random_u64() >> 11) as f64 / (1u64 << 53) as f64
}

/// A random ID like the JS client's `nanoid`.
pub(crate) fn random_id() -> String {
    const ALPHABET: &[u8; 64] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_-";
    (0..21)
        .map(|_| ALPHABET[(random_u64() % 64) as usize] as char)
        .collect()
}

/// `Date.prototype.toISOString` of a time in milliseconds since 1970.
pub(crate) fn iso_date_string(ms: f64) -> String {
    let t = ms.floor() as i64;
    let mut days = t.div_euclid(86_400_000);
    let ms_of_day = t.rem_euclid(86_400_000);
    // Howard Hinnant's civil_from_days.
    days += 719_468;
    let era = days.div_euclid(146_097);
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        ms_of_day / 3_600_000,
        ms_of_day / 60_000 % 60,
        ms_of_day / 1000 % 60,
        ms_of_day % 1000
    )
}

/// The message valita gives for a message that doesn't parse (`getMessage`
/// in `packages/shared/src/valita.ts`), for the cases the client can tell
/// apart.
fn valita_message(e: &DecodeError, text: &str) -> String {
    if e.message.starts_with("Unknown message type") || e.message == "Expected [tag, body] tuple" {
        return format!("Invalid union value: {text}");
    }
    e.valita_message()
}

/// The options of a Zero client (`ZeroOptions` in
/// `packages/zero-client/src/client/options.ts`).
#[derive(Clone)]
pub struct ZeroOptions {
    /// The zero-cache URL, `http` or `https`. Without one the client runs
    /// locally and never connects.
    pub cache_url: Option<String>,
    /// The user the client syncs for. `None` for a logged-out client.
    pub user_id: Option<String>,
    /// The auth token zero-cache and the API server see.
    pub auth: Option<String>,
    /// Distinguishes local stores of the same user and URLs.
    pub storage_key: Option<String>,
    /// How long the client waits for a pong before it reconnects.
    pub ping_timeout_ms: f64,
    /// How long the client tries to reconnect before the state is
    /// `Disconnected`.
    pub disconnect_timeout_ms: f64,
    /// How long query changes are batched before they are sent.
    pub query_change_throttle_ms: f64,
    /// The longest `Sec-WebSocket-Protocol` header the client sends.
    /// `initConnection` is sent as a message instead when it doesn't fit.
    pub max_header_length: usize,
    /// The API server's push URL, when it differs from the one zero-cache is
    /// configured with.
    pub mutate_url: Option<String>,
    /// Headers zero-cache sends to `mutate_url`.
    pub mutate_headers: Option<Vec<(String, String)>>,
    /// The API server's query URL, when it differs from the one zero-cache is
    /// configured with.
    pub query_url: Option<String>,
    /// Headers zero-cache sends to `query_url`.
    pub query_headers: Option<Vec<(String, String)>>,
    /// The server needs a newer client. Defaults to doing nothing.
    pub on_update_needed: Option<UpdateNeededHandler>,
    /// The server no longer has the client's state, or the stored replica
    /// didn't decode and the local store was reset. Without it, a client
    /// the server lost replaces itself by a new one on the same store, as
    /// JS reloads the page (see [`Zero::new`]).
    ///
    /// [`Zero::new`]: crate::client::Zero::new
    pub on_client_state_not_found: Option<Rc<dyn Fn()>>,
    /// Where the client keeps its replica. Defaults to
    /// [`ZeroStorage::Memory`], which keeps nothing across restarts.
    pub storage: ZeroStorage,
    /// The local store failed, so the client runs on what it has in memory
    /// and persists nothing more (`onStorageFailure`). Called once.
    pub on_storage_failure: Option<StorageFailureHandler>,
    /// The least severe messages logged.
    pub log_level: LogLevel,
    /// Where messages are logged. Defaults to [`StderrLogSink`].
    pub log_sink: Option<Rc<dyn LogSink>>,
    /// How long the app is hidden before the client disconnects
    /// (`hiddenTabDisconnectDelay`); see [`Zero::set_visible`].
    ///
    /// [`Zero::set_visible`]: crate::client::Zero::set_visible
    pub hidden_disconnect_delay_ms: f64,
}

impl Default for ZeroOptions {
    fn default() -> Self {
        ZeroOptions {
            cache_url: None,
            user_id: None,
            auth: None,
            storage_key: None,
            ping_timeout_ms: 5_000.0,
            disconnect_timeout_ms: 60_000.0,
            query_change_throttle_ms: 10.0,
            max_header_length: 8 * 1024,
            mutate_url: None,
            mutate_headers: None,
            query_url: None,
            query_headers: None,
            on_update_needed: None,
            on_client_state_not_found: None,
            storage: ZeroStorage::Memory,
            on_storage_failure: None,
            log_level: LogLevel::Warn,
            log_sink: None,
            hidden_disconnect_delay_ms: 0.0,
        }
    }
}

/// Called when the local store fails (`onStorageFailure`).
pub type StorageFailureHandler = Rc<dyn Fn(&StorageFailure)>;

/// Called when the server needs a newer client (`onUpdateNeeded`).
pub type UpdateNeededHandler = Rc<dyn Fn(&UpdateNeededReason)>;

/// Why the client must be updated (`UpdateNeededReason`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateNeededReason {
    /// `VersionNotSupported`, `SchemaVersionNotSupported`, or
    /// `NewClientGroup`.
    pub type_: String,
    /// The server's explanation, if it gave one.
    pub message: Option<String>,
}

impl UpdateNeededReason {
    /// The JS reason object.
    pub fn to_json(&self) -> Value {
        let mut o = Object::new();
        o.insert("type", Value::String(self.type_.clone()));
        if let Some(m) = &self.message {
            o.insert("message", Value::String(m.clone()));
        }
        Value::Object(o)
    }
}

/// How a mutation ended, on the client or on the server
/// (`MutatorResultDetails` in `packages/zero-client/src/client/custom.ts`).
#[derive(Clone, Debug, PartialEq)]
pub enum MutationOutcome {
    /// The mutation ran.
    Success {
        /// What the mutator returned (client) or the server's result.
        data: Option<Value>,
    },
    /// The mutator or the API server raised an [`ApplicationError`].
    AppError {
        /// The error's message.
        message: String,
        /// The error's details.
        details: Option<Value>,
    },
    /// Zero couldn't run the mutation or learn its result, such as while
    /// offline or when the connection closed. On the server side this
    /// doesn't mean the mutation wasn't applied: the client sends pending
    /// mutations again when it reconnects.
    ZeroError {
        /// Why.
        message: String,
    },
}

impl MutationOutcome {
    /// The JS result object.
    pub fn to_json(&self) -> Value {
        let mut o = Object::new();
        match self {
            MutationOutcome::Success { data } => {
                o.insert("type", Value::from("success"));
                if let Some(d) = data {
                    o.insert("data", d.clone());
                }
            }
            MutationOutcome::AppError { message, details } => {
                let mut e = Object::new();
                e.insert("type", Value::from("app"));
                e.insert("message", Value::String(message.clone()));
                if let Some(d) = details {
                    e.insert("details", d.clone());
                }
                o.insert("type", Value::from("error"));
                o.insert("error", Value::Object(e));
            }
            MutationOutcome::ZeroError { message } => {
                let mut e = Object::new();
                e.insert("type", Value::from("zero"));
                e.insert("message", Value::String(message.clone()));
                o.insert("type", Value::from("error"));
                o.insert("error", Value::Object(e));
            }
        }
        Value::Object(o)
    }
}

/// What `mutate` returns: the outcome of the optimistic run on the client,
/// and the server's.
#[derive(Clone)]
pub struct MutatorResult {
    /// Settles when the mutator has run on the client: what it returned, or
    /// what it raised.
    pub client: Deferred<MutationOutcome>,
    /// Settles when the server has run the mutation, or when the client can't
    /// learn its result.
    pub server: Deferred<MutationOutcome>,
}

/// A mutator bound to the app's context: runs it in a transaction.
pub(crate) type MutatorFn =
    Rc<dyn Fn(&Transaction, Option<&Value>) -> Result<Option<Value>, Error>>;

/// A connection's readiness, as the pusher waits on it
/// (`#connectionReadyResolver`).
#[derive(Clone, Default)]
struct ReadyPromise(Rc<RefCell<ReadyState>>);

#[derive(Default)]
struct ReadyState {
    settled: Option<bool>,
    waiters: Vec<Box<dyn FnOnce(bool)>>,
}

impl ReadyPromise {
    fn is(&self, other: &ReadyPromise) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    fn settle(&self, value: bool, event_loop: &EventLoop) {
        let waiters = {
            let mut s = self.0.borrow_mut();
            if s.settled.is_some() {
                return;
            }
            s.settled = Some(value);
            std::mem::take(&mut s.waiters)
        };
        for w in waiters {
            event_loop.microtask(move || w(value));
        }
    }

    /// Calls `waiter` in a microtask once settled.
    fn on_settle(&self, event_loop: &EventLoop, waiter: impl FnOnce(bool) + 'static) {
        let settled = self.0.borrow().settled;
        match settled {
            Some(v) => event_loop.microtask(move || waiter(v)),
            None => self.0.borrow_mut().waiters.push(Box::new(waiter)),
        }
    }
}

#[derive(Default)]
struct ConnectAttempt {
    events: RefCell<Vec<(f64, String)>>,
    aborted: Cell<bool>,
    abort_reason: RefCell<Option<ConnectionError>>,
    timeout: RefCell<Option<Box<dyn Timer>>>,
    finished: Cell<bool>,
    on_connected: RefCell<Option<Box<dyn FnOnce()>>>,
    /// When the socket was opened, for the open and `connected` logs.
    start: Cell<f64>,
}

impl ConnectAttempt {
    fn cancel_timeout(&self) {
        if let Some(t) = self.timeout.borrow_mut().take() {
            t.cancel();
        }
    }
}

/// A pending mutation's identity.
type MutationKey = (String, u64);

fn mutation_key(client_id: &str, id: f64) -> MutationKey {
    (client_id.to_owned(), id.to_bits())
}

/// The replica as JS's last persist left it in the store: the snapshot it
/// persisted (by cookie), its main map over that snapshot, the keys where
/// the two may differ, and the pending mutations it ran.
#[derive(Default)]
struct PersistedState {
    cookie: Value,
    main: KvMap,
    touched: HashSet<String>,
    mutations: HashSet<MutationKey>,
}

/// The engine's end of one socket.
pub(crate) struct SocketHandle {
    me: Weak<SocketHandle>,
    engine: Weak<Engine>,
    connection: RefCell<Option<Box<dyn SocketConnection>>>,
    closed: Cell<bool>,
}

impl SocketHandle {
    fn new(engine: Weak<Engine>) -> Rc<Self> {
        Rc::new_cyclic(|me| SocketHandle {
            me: me.clone(),
            engine,
            connection: RefCell::new(None),
            closed: Cell::new(false),
        })
    }

    fn send(&self, text: &str) {
        if self.closed.get() {
            return;
        }
        if let Some(c) = &*self.connection.borrow() {
            c.send(text);
        }
    }

    fn close(&self, code: Option<u16>) {
        if self.closed.replace(true) {
            return;
        }
        let connection = self.connection.borrow_mut().take();
        if let Some(c) = connection {
            c.close(code);
        }
    }
}

impl SocketEvents for SocketHandle {
    fn did_open(&self) {
        if self.closed.get() {
            return;
        }
        if let (Some(e), Some(me)) = (self.engine.upgrade(), self.me.upgrade()) {
            e.socket_did_open(&me);
        }
    }

    fn did_receive(&self, message: SocketMessage) {
        if self.closed.get() {
            return;
        }
        if let (Some(e), Some(me)) = (self.engine.upgrade(), self.me.upgrade()) {
            e.socket_did_receive(&me, message);
        }
    }

    fn did_close(&self, code: u16, reason: &str, was_clean: bool) {
        if self.closed.get() {
            return;
        }
        if let (Some(e), Some(me)) = (self.engine.upgrade(), self.me.upgrade()) {
            e.socket_did_close(&me, code, reason, was_clean);
        }
    }
}

type VisibilityWaiter = (bool, u64, Box<dyn FnOnce()>);

pub(crate) struct Engine {
    me: Weak<Engine>,
    pub(crate) schema: Arc<Schema>,
    pub(crate) options: ZeroOptions,
    pub(crate) client_id: String,
    pub(crate) client_group_id: String,
    profile_id: String,
    pub(crate) user_id: Option<String>,
    pub(crate) event_loop: Rc<EventLoop>,
    scheduler: Rc<dyn Scheduler>,
    pub(crate) log: Log,
    socket_factory: Option<Rc<dyn SocketFactory>>,
    socket_origin: Option<String>,
    mutators: Rc<HashMap<String, MutatorFn>>,
    client_schema: crate::protocol::ClientSchema,

    store: RefCell<ReplicaStore>,
    pub(crate) database: Rc<ReplicaDatabase>,
    lifecycle_timers: RefCell<Vec<Box<dyn Timer>>>,
    ivm_main: Rc<IvmBranch>,
    pub(crate) context: Rc<QueryContext>,
    pub(crate) connection_manager: Rc<ConnectionManager>,
    mutation_tracker: Rc<MutationTracker>,
    query_manager: Rc<QueryManager>,
    poke_handler: RefCell<PokeHandler>,

    /// The current token: `options.auth`, then the last one `connect`
    /// set; empty for none.
    pub(crate) auth: RefCell<String>,
    rep_closed: Cell<bool>,
    storage_failure_reported: Cell<bool>,
    socket: RefCell<Option<Rc<SocketHandle>>>,
    connect_cookie: RefCell<Value>,
    init_connection_queries: RefCell<Option<OrderedMap<UpQueriesPatchOp>>>,
    /// Deleted clients still to tell the server about on this connection.
    deleted_clients: RefCell<Option<DeleteClientsBody>>,
    last_mutation_id_received: Cell<f64>,
    last_mutation_ids_sent: RefCell<HashMap<String, f64>>,
    persisted: RefCell<PersistedState>,
    persist_timer: RefCell<Option<Box<dyn Timer>>>,
    last_persist_at: Cell<Option<f64>>,
    additional_connect_params: RefCell<Option<Vec<(String, String)>>>,
    /// When the current connection got the server's `connected`.
    connected_at: Cell<Option<f64>>,
    /// Consecutive transport failures, which set the reconnect backoff.
    transport_failures: Cell<u32>,
    current_attempt: RefCell<Option<Rc<ConnectAttempt>>>,
    connection_ready: RefCell<ReadyPromise>,
    recent_sent_messages: RefCell<VecDeque<Value>>,
    mutation_rejection: RefCell<Option<ConnectionError>>,
    client_group_disabled: Cell<bool>,

    // The run loop's current wait. Each wait gets a new token, so a wait
    // that was superseded does nothing when it ends.
    loop_token: Cell<u64>,
    abort_ping_timeout: RefCell<Rc<dyn Fn()>>,
    on_pong: RefCell<Rc<dyn Fn()>>,
    /// Pings the server now, when the run loop waits while connected.
    ping_now: RefCell<Rc<dyn Fn()>>,
    /// The run loop's backoff before it reconnects.
    backoff: RefCell<Option<(u64, Box<dyn Timer>)>>,

    // The push loop (Replicache's push `ConnectionLoop`).
    push_timer: RefCell<Option<Box<dyn Timer>>>,
    push_in_flight: Cell<bool>,
    push_requested: Cell<bool>,
    push_request_counter: Cell<u64>,

    /// Trails the app's visibility by `hidden_disconnect_delay_ms` when it
    /// is hidden (`DocumentVisibilityWatcher`).
    pub(crate) visible: Cell<bool>,
    hidden_timer: RefCell<Option<Box<dyn Timer>>>,
    visibility_waiters: RefCell<Vec<VisibilityWaiter>>,
    pending_waiters: RefCell<Vec<Box<dyn FnOnce()>>>,

    /// Set while a mutator runs, for a mutation or a rebase: Replicache's
    /// write lock. Mutations that start meanwhile wait, in order.
    write_locked: Cell<bool>,
    waiting_for_write_lock: RefCell<VecDeque<Box<dyn FnOnce()>>>,

    /// What `Zero` does when the client can't sync any longer: by default
    /// `ZeroOptions::on_client_state_not_found`.
    pub(crate) on_client_state_not_found: RefCell<Option<Rc<dyn Fn()>>>,
    /// What `Zero` does when the app must be updated to sync, besides
    /// calling `ZeroOptions::on_update_needed`.
    pub(crate) on_update_needed: RefCell<Option<UpdateNeededHandler>>,
}

fn noop() -> Rc<dyn Fn()> {
    Rc::new(|| {})
}

/// `JSON.stringify(recentSentMessages, null, 2)`.
fn pretty_recent_messages(messages: &VecDeque<Value>) -> String {
    let entries: Vec<String> = messages
        .iter()
        .map(|m| {
            let fields: Vec<String> = ["type", "size", "snippet"]
                .iter()
                .filter_map(|k| {
                    m.get(k).map(|v| {
                        format!(
                            "    {}: {}",
                            json::stringify(&Value::from(*k)),
                            json::stringify(v)
                        )
                    })
                })
                .collect();
            format!("  {{\n{}\n  }}", fields.join(",\n"))
        })
        .collect();
    format!("[\n{}\n]", entries.join(",\n"))
}

fn bad_order_message(name: &str, received: &str, last: &str) -> String {
    format!(
        "Received {name} {received} is < than last snapshot {name} {last}; ignoring client view"
    )
}

/// The synced aggregate rows `write` wrote, with their values now.
fn aggregate_writes(write: &KvWrite) -> AggregateWrites {
    let mut writes: AggregateWrites = write
        .writes
        .iter()
        .filter(|k| k.starts_with(AGGREGATES_PREFIX))
        .map(|k| (k.clone(), write.get(k).cloned()))
        .collect();
    writes.sort_by(|a, b| a.0.cmp(&b.0));
    writes
}

impl Engine {
    pub(crate) fn new(
        schema: Arc<Schema>,
        options: ZeroOptions,
        mutators: Rc<HashMap<String, MutatorFn>>,
        base: Rc<dyn Scheduler>,
        socket_factory: Option<Rc<dyn SocketFactory>>,
        visible: bool,
    ) -> Result<Rc<Engine>, Error> {
        if options.user_id.as_deref() == Some("") {
            return Err(ConnectionError::client(
                ClientErrorKind::Internal,
                "ZeroOptions.userID should not be empty. Omit it entirely for logged-out clients.",
            )
            .into());
        }
        Self::check_auth_valid(options.user_id.as_deref(), options.auth.as_deref())?;
        let socket_origin = options
            .cache_url
            .as_deref()
            .map(Self::socket_origin)
            .transpose()?;
        let event_loop = EventLoop::new(base.clone());
        let scheduler: Rc<dyn Scheduler> = Rc::new(LoopScheduler {
            base: base.clone(),
            event_loop: Rc::downgrade(&event_loop),
        });
        let client_id = random_id();
        let sink = options
            .log_sink
            .clone()
            .unwrap_or_else(|| Rc::new(StderrLogSink));
        let log = Log::new(options.log_level, sink).with("clientID", &client_id);
        let client_schema = schema.client_schema();
        let schema_hash = base36(xxhash32::h64(&json::stringify(
            &client_schema.normalized().to_json(),
        )));

        // Open the database and join a client group (`initClientV6`).
        let name = ReplicaDatabase::name(
            options.user_id.as_deref(),
            options.storage_key.as_deref().unwrap_or(""),
            options.mutate_url.as_deref(),
            options.query_url.as_deref(),
            &schema_hash,
        );
        let mut mutator_names: Vec<String> = mutators.keys().cloned().collect();
        mutator_names.sort();
        let (mut database, mut open_failure, mut corrupt) =
            ReplicaDatabase::open(&options.storage, &name);
        let joined = match database.init_client(&client_id, &mutator_names, base.now()) {
            // What the client group holds doesn't decode, so the client
            // state is lost: drop the database and start a new one
            // (`#handleInvalidRefCount`).
            Err(LoadError::Corrupt(_)) => {
                corrupt = true;
                database.drop_database();
                database.release();
                (database, open_failure, _) = ReplicaDatabase::open(&options.storage, &name);
                database.init_client(&client_id, &mutator_names, base.now())
            }
            r => r,
        };
        let (client_group_id, group) = match joined {
            Ok(j) => j,
            // The client group can't be read: run on memory, as when the
            // open fails.
            Err(e) => {
                database.release();
                database = ReplicaDatabase::fallback(&name, &options.storage);
                open_failure = Some(match e {
                    LoadError::Failure(f) => f,
                    LoadError::Corrupt(m) => {
                        StorageFailure::new(super::storage::StorageFailureKind::IoError, m)
                    }
                });
                database
                    .init_client(&client_id, &mutator_names, base.now())
                    .expect("a memory database reads")
            }
        };
        let profile_id = database.profile_id();
        // Drop stored mutation IDs that aren't mutation IDs (a corrupt
        // store) rather than trust them.
        let is_mutation_id = |id: f64| id >= 0.0 && number::is_safe_integer(id);
        let last_mutation_ids = group
            .last_mutation_ids
            .into_iter()
            .filter(|(cid, lmid)| {
                let ok = is_mutation_id(*lmid);
                if !ok {
                    log.error(|| {
                        format!(
                            "Dropping invalid stored last mutation ID of {cid}: {}",
                            number::format(*lmid)
                        )
                    });
                }
                ok
            })
            .collect();
        let mutations: Vec<StoredMutation> = group
            .mutations
            .into_iter()
            .filter(|m| {
                let ok = is_mutation_id(m.id);
                if !ok {
                    log.error(|| {
                        format!(
                            "Dropping stored mutation \"{}\" of {} with invalid ID {}",
                            m.name,
                            m.client_id,
                            number::format(m.id)
                        )
                    });
                }
                ok
            })
            .collect();
        let store = ReplicaStore::new(
            Snapshot {
                map: group.entries,
                cookie: group.cookie,
                last_mutation_ids,
            },
            mutations
                .iter()
                .map(|m| PendingMutation {
                    id: m.id,
                    client_id: m.client_id.clone(),
                    name: m.name.clone(),
                    args: m.args.clone(),
                    timestamp: m.timestamp,
                    writes: HashSet::new(),
                })
                .collect(),
        );
        let stored_aggregate_writes: HashMap<MutationKey, AggregateWrites> = mutations
            .into_iter()
            .map(|m| (mutation_key(&m.client_id, m.id), m.aggregate_writes))
            .collect();
        let ivm_main = IvmBranch::new(schema.clone());
        let connection_manager = ConnectionManager::new(
            options.disconnect_timeout_ms,
            scheduler.clone(),
            Rc::downgrade(&event_loop),
        );

        let engine = Rc::new_cyclic(|me: &Weak<Engine>| {
            let mutation_tracker = {
                let (a, f, q) = (me.clone(), me.clone(), me.clone());
                Rc::new(MutationTracker::new(
                    client_id.clone(),
                    Box::new(move |id| {
                        if let Some(e) = a.upgrade() {
                            e.send_if_connected(Upstream::AckMutationResponses(id));
                        }
                    }),
                    Box::new(move |error| {
                        if let Some(e) = f.upgrade() {
                            e.disconnect(error, None);
                        }
                    }),
                    Box::new(move || {
                        if let Some(e) = q.upgrade() {
                            e.query_manager.on_all_mutations_applied();
                        }
                    }),
                ))
            };
            let context = {
                let (a, u, c) = (me.clone(), me.clone(), me.clone());
                let l = event_loop.clone();
                QueryContext::new(
                    ivm_main.clone(),
                    Box::new(move |id, ttl, got| match a.upgrade() {
                        Some(e) => e
                            .query_manager
                            .add_custom(&id.name, &id.args, ttl, Some(got)),
                        None => Box::new(|| {}),
                    }),
                    Rc::new(move |id, ttl| {
                        if let Some(e) = u.upgrade() {
                            e.query_manager.update_custom(&id.name, &id.args, ttl);
                        }
                    }),
                    Rc::new(move |f| l.microtask(f)),
                    Some(log.clone()),
                    Box::new(move || match c.upgrade()?.connection_manager.state() {
                        State::Closed(r) => Some(r),
                        _ => None,
                    }),
                    true,
                )
            };
            let query_manager = {
                let (s, p) = (me.clone(), me.clone());
                QueryManager::new(
                    client_id.clone(),
                    scheduler.clone(),
                    options.query_change_throttle_ms,
                    Box::new(move |body| {
                        if let Some(e) = s.upgrade() {
                            e.send_if_connected(Upstream::ChangeDesiredQueries(body));
                        }
                    }),
                    Box::new(move || p.upgrade().map_or(0, |e| e.mutation_tracker.size())),
                )
            };
            Engine {
                me: me.clone(),
                poke_handler: RefCell::new(PokeHandler::new(client_id.clone(), schema.clone())),
                schema: schema.clone(),
                user_id: options.user_id.clone(),
                // Set now, as JS's constructor does, so a token from
                // `connect` right after init isn't replaced when the run
                // loop starts.
                auth: RefCell::new(options.auth.clone().unwrap_or_default()),
                options: options.clone(),
                client_id: client_id.clone(),
                client_group_id: client_group_id.clone(),
                profile_id,
                event_loop: event_loop.clone(),
                scheduler: scheduler.clone(),
                log: log.clone(),
                socket_factory,
                socket_origin,
                mutators,
                client_schema,
                store: RefCell::new(store),
                database: database.clone(),
                lifecycle_timers: RefCell::default(),
                ivm_main: ivm_main.clone(),
                context,
                connection_manager: connection_manager.clone(),
                mutation_tracker,
                query_manager,
                rep_closed: Cell::new(false),
                storage_failure_reported: Cell::new(false),
                socket: RefCell::new(None),
                connect_cookie: RefCell::new(Value::Null),
                init_connection_queries: RefCell::new(None),
                deleted_clients: RefCell::new(None),
                last_mutation_id_received: Cell::new(0.0),
                last_mutation_ids_sent: RefCell::default(),
                persisted: RefCell::default(),
                persist_timer: RefCell::new(None),
                last_persist_at: Cell::new(None),
                additional_connect_params: RefCell::new(None),
                connected_at: Cell::new(None),
                transport_failures: Cell::new(0),
                current_attempt: RefCell::new(None),
                connection_ready: RefCell::default(),
                recent_sent_messages: RefCell::default(),
                mutation_rejection: RefCell::new(None),
                client_group_disabled: Cell::new(false),
                loop_token: Cell::new(0),
                abort_ping_timeout: RefCell::new(noop()),
                on_pong: RefCell::new(noop()),
                ping_now: RefCell::new(noop()),
                backoff: RefCell::new(None),
                push_timer: RefCell::new(None),
                push_in_flight: Cell::new(false),
                push_requested: Cell::new(false),
                push_request_counter: Cell::new(0),
                visible: Cell::new(visible),
                hidden_timer: RefCell::new(None),
                visibility_waiters: RefCell::default(),
                pending_waiters: RefCell::default(),
                write_locked: Cell::new(false),
                waiting_for_write_lock: RefCell::default(),
                on_client_state_not_found: RefCell::new(None),
                on_update_needed: RefCell::new(None),
            }
        });

        let e = &engine;
        {
            let w = Rc::downgrade(e);
            // The closure is dropped with the manager; no need to keep the
            // unsubscribe.
            let _ = connection_manager.subscribe(move |state| {
                if let (Some(e), State::Closed(reason)) = (w.upgrade(), state) {
                    e.query_manager.handle_closed(reason);
                }
            });
            let w = Rc::downgrade(e);
            let _ = connection_manager.subscribe(move |state| {
                if let Some(e) = w.upgrade() {
                    e.on_connection_state_change(state);
                }
            });
        }
        e.on_connection_state_change(&connection_manager.state());

        // Load the snapshot into the sources and replay the pending
        // mutations on it, with the synced aggregate rows they wrote when
        // they were last persisted, as JS's stored commits have them.
        let initial = diff_all(&KvMap::default(), &e.store.borrow().snapshot.map);
        e.ivm_main.apply(&initial, &HashMap::new());
        let pending = e.store.borrow().pending.clone();
        if !pending.is_empty() {
            let snapshot_map = e.store.borrow().snapshot.map.clone();
            let (main, replayed, touched, _) = e.replay(
                &pending,
                &snapshot_map,
                e.ivm_main.fork(),
                &stored_aggregate_writes,
                &mut |_, _| {},
            );
            let diff = {
                let mut store = e.store.borrow_mut();
                let snapshot = store.snapshot.clone();
                store.advance(snapshot, main, replayed, Some(&touched))
            };
            e.ivm_main.apply(&diff, &HashMap::new());
        }
        {
            let store = e.store.borrow();
            *e.persisted.borrow_mut() = PersistedState {
                cookie: store.snapshot.cookie.clone(),
                main: store.main.clone(),
                touched: store.pending_writes(),
                mutations: store
                    .pending
                    .iter()
                    .map(|m| mutation_key(&m.client_id, m.id))
                    .collect(),
            };
        }
        {
            let w = Rc::downgrade(e);
            database.add_failure_listener(
                &e.client_id,
                Rc::new(move |f| {
                    if let Some(e) = w.upgrade() {
                        e.storage_failed(f, false);
                    }
                }),
            );
        }
        // Another client of the group stopped (the `onDelete` of
        // `makeActiveClientsManager`): tell the server, if connected.
        {
            let w = Rc::downgrade(e);
            database.client_started(
                &e.client_id,
                &e.client_group_id,
                Rc::new(move |id| {
                    if let Some(e) = w.upgrade() {
                        let id = id.to_owned();
                        e.event_loop.clone().dispatch(|| {
                            e.send_if_connected(Upstream::DeleteClients(DeleteClientsBody {
                                client_ids: Some(vec![id]),
                                client_group_ids: None,
                            }))
                        });
                    }
                }),
            );
        }
        if corrupt {
            // This client already starts on the new database, which is what
            // replacing it would do; the app's callback still hears it, as
            // in JS.
            e.log.error(|| {
                format!("Client state is corrupt; dropped database {name} and started a new one")
            });
            // Only the app's callback: this client is already on a new
            // database, so it isn't replaced (as in Swift).
            e.micro(|e| {
                if let Some(f) = &e.options.on_client_state_not_found {
                    f();
                }
            });
        }
        if let Some(f) = open_failure {
            e.log.warn(|| {
                format!(
                    "Local store storage failed ({}) opening {name}; running this instance in memory: {}",
                    f.kind.as_str(),
                    f.message
                )
            });
            e.micro(move |e| e.storage_failed(&f, true));
        }
        e.start_lifecycle();

        // The replica is loaded: report its got queries and mutation
        // results, as the first runs of the store watches do, then build
        // the pipelines.
        let main = e.store.borrow().main.clone();
        e.fire_watchers(&diff_all(&KvMap::default(), &main), true);
        e.context.mark_pipelines_ready();
        e.micro(|e| e.start_run_loop());
        Ok(engine)
    }

    fn check_auth_valid(user_id: Option<&str>, auth: Option<&str>) -> Result<(), Error> {
        if user_id.is_none() && auth.is_some_and(|a| !a.is_empty()) {
            return Err(ConnectionError::client(
                ClientErrorKind::Internal,
                "ZeroOptions.userID is required when auth is set.",
            )
            .into());
        }
        Ok(())
    }

    /// The WebSocket origin of the zero-cache URL (`getServer`,
    /// `toWSString`).
    fn socket_origin(url: &str) -> Result<String, Error> {
        let (scheme, rest) = if let Some(rest) = url.strip_prefix("http://") {
            ("ws://", rest)
        } else if let Some(rest) = url.strip_prefix("https://") {
            ("wss://", rest)
        } else {
            return Err(ConnectionError::client(
                ClientErrorKind::Internal,
                "ZeroOptions.cacheURL must use the \"http\" or \"https\" scheme.",
            )
            .into());
        };
        let mut rest = rest.to_owned();
        if !rest.contains('/') {
            rest.push('/');
        }
        Ok(format!("{scheme}{rest}"))
    }

    fn micro(&self, f: impl FnOnce(&Engine) + 'static) {
        let w = self.me.clone();
        self.event_loop.microtask(move || {
            if let Some(e) = w.upgrade() {
                f(&e);
            }
        });
    }

    fn later(&self, ms: f64, f: impl FnOnce(&Engine) + 'static) -> Box<dyn Timer> {
        let w = self.me.clone();
        self.scheduler.schedule(
            ms,
            Box::new(move || {
                if let Some(e) = w.upgrade() {
                    f(&e);
                }
            }),
        )
    }

    fn status_is(&self, status: ConnectionStatus) -> bool {
        self.connection_manager.is(status)
    }

    // MARK: Sending

    fn send_if_connected(&self, message: Upstream) {
        if self.status_is(ConnectionStatus::Connected) {
            self.send(message);
        }
    }

    fn send(&self, message: Upstream) {
        let Some(socket) = self.socket.borrow().clone() else {
            return;
        };
        let text = message.to_text();
        if message.tag() != "ping" {
            let units: Vec<u16> = text.encode_utf16().collect();
            let snippet = if units.len() > 200 {
                String::from_utf16(&units[..200]).unwrap_or_else(|_| text.clone())
            } else {
                text.clone()
            };
            let mut o = Object::new();
            o.insert("type", Value::from(message.tag()));
            o.insert("size", Value::Number(units.len() as f64));
            o.insert("snippet", Value::String(snippet));
            let mut recent = self.recent_sent_messages.borrow_mut();
            if recent.len() >= 5 {
                recent.pop_front();
            }
            recent.push_back(Value::Object(o));
        }
        socket.send(&text);
    }

    // MARK: The run loop

    fn start_run_loop(&self) {
        if self.socket_origin.is_none() {
            self.connection_manager
                .disconnected(ConnectionError::client(
                    ClientErrorKind::NoSocketOrigin,
                    "No server socket origin provided",
                ));
            return;
        }
        self.connection_manager.initialized();
        self.iterate();
    }

    fn next_loop_token(&self) -> u64 {
        let t = self.loop_token.get() + 1;
        self.loop_token.set(t);
        t
    }

    fn is_token(&self, token: u64) -> bool {
        self.loop_token.get() == token
    }

    /// One turn of the JS run loop: acts on the connection status.
    fn iterate(&self) {
        if !self.connection_manager.should_continue_run_loop() {
            return;
        }
        let token = self.next_loop_token();
        match self.connection_manager.status() {
            ConnectionStatus::Connecting | ConnectionStatus::Disconnected => {
                if !self.visible.get() {
                    // Wait for the app to come back, or for a state change.
                    self.on_visibility(true, token, |e| e.begin_attempt());
                    self.on_state_change(token, |e, state| e.resume_after_state_change(state));
                    return;
                }
                // The app is visible, which wins the race with a state
                // change.
                self.micro(move |e| {
                    if e.is_token(token) {
                        e.begin_attempt();
                    }
                });
            }
            ConnectionStatus::Connected => self.wait_while_connected(token),
            ConnectionStatus::NeedsAuth | ConnectionStatus::Error => {
                let status = self.connection_manager.status();
                self.log.info(|| {
                    format!(
                        "Run loop paused in {} state. Call zero.connection.connect() to resume.",
                        status.as_str()
                    )
                });
                self.wait_for_connect_request(token);
            }
            ConnectionStatus::Closed => {}
            ConnectionStatus::Initializing => unreachable!("The run loop runs once initialized"),
        }
    }

    /// Ends the current wait of the run loop with `error` (the `catch` of
    /// the JS run loop).
    fn loop_failed(&self, error: ConnectionError) {
        let (transition, reason) = ConnectionTransition::of(&error);
        if !self.status_is(ConnectionStatus::Connected)
            && error.client_kind() != Some(ClientErrorKind::ClientClosed)
        {
            // Only errors that stop the run loop are errors. Reconnects and
            // auth failures are routine.
            let mut level = match transition {
                ConnectionTransition::Error => LogLevel::Error,
                ConnectionTransition::Disconnected | ConnectionTransition::Closed => LogLevel::Info,
                ConnectionTransition::None | ConnectionTransition::NeedsAuth => LogLevel::Warn,
            };
            if error.server_kind() == Some(ErrorKind::ClientNotFound) {
                level = LogLevel::Warn;
            }
            self.log
                .log(level, || format!("Failed to connect: {}", error.message()));
        }
        let mut backoff_ms = RUN_LOOP_INTERVAL_MS;
        let mut sleep_ms = None;
        match transition {
            ConnectionTransition::None => {
                // A transport failure is retried soon, then less often.
                // Errors the server sent keep the full interval, adjusted by
                // any backoff they carry.
                if reason.is_transport_failure() {
                    let failures = self.transport_failures.get() + 1;
                    self.transport_failures.set(failures);
                    backoff_ms = reconnect_backoff_ms(failures, RUN_LOOP_INTERVAL_MS, random_f64());
                }
                if let Some(ErrorBody::Backoff {
                    min_backoff_ms,
                    max_backoff_ms,
                    reconnect_params,
                    ..
                }) = reason.backoff()
                {
                    if let Some(min) = min_backoff_ms {
                        backoff_ms = backoff_ms.max(*min);
                    }
                    if let Some(max) = max_backoff_ms {
                        backoff_ms = backoff_ms.min(*max);
                    }
                    *self.additional_connect_params.borrow_mut() =
                        reconnect_params.as_ref().map(|params| {
                            let mut p = params.clone();
                            p.sort_by(|a, b| a.0.cmp(&b.0));
                            p
                        });
                }
                sleep_ms = Some(backoff_ms);
            }
            ConnectionTransition::NeedsAuth => self.connection_manager.needs_auth(reason),
            ConnectionTransition::Error => self.connection_manager.error(reason),
            ConnectionTransition::Disconnected => self.connection_manager.disconnected(reason),
            ConnectionTransition::Closed => {}
        }
        let token = self.next_loop_token();
        match sleep_ms {
            Some(ms) if ms > 0.0 => {
                let timer = self.later(ms, move |e| {
                    if e.is_token(token) {
                        e.backoff.borrow_mut().take();
                        e.iterate();
                    }
                });
                *self.backoff.borrow_mut() = Some((token, timer));
            }
            _ => self.micro(move |e| {
                if e.is_token(token) {
                    e.iterate();
                }
            }),
        }
    }

    /// `throwIfConnectionError`: whether a state the run loop wakes up to
    /// stops it. Dropped sockets and timeouts just reconnect.
    fn connection_error(state: &State) -> Option<ConnectionError> {
        match state {
            State::NeedsAuth(r) | State::Error(r) | State::Closed(r) => Some(r.clone()),
            State::Connecting {
                reason: Some(r), ..
            }
            | State::Disconnected(r) => match r.client_kind() {
                Some(
                    ClientErrorKind::ConnectTimeout
                    | ClientErrorKind::AbruptClose
                    | ClientErrorKind::CleanClose
                    | ClientErrorKind::Hidden,
                ) => None,
                _ => Some(r.clone()),
            },
            _ => None,
        }
    }

    /// Wakes the loop on the next state change, unless the wait is over.
    fn on_state_change(&self, token: u64, body: impl FnOnce(&Engine, State) + 'static) {
        let (a, b) = (self.me.clone(), self.me.clone());
        self.connection_manager.on_next_state_change(
            move || a.upgrade().is_some_and(|e| e.is_token(token)),
            move |state| {
                if let Some(e) = b.upgrade()
                    && e.is_token(token)
                {
                    body(&e, state);
                }
            },
        );
    }

    fn resume_after_state_change(&self, state: State) {
        match Self::connection_error(&state) {
            Some(error) => self.loop_failed(error),
            None => self.iterate(),
        }
    }

    /// Connected: ping after `ping_timeout_ms` without poke traffic.
    fn wait_while_connected(&self, token: u64) {
        let ready = self.connection_ready.borrow().clone();
        let timer: Rc<RefCell<Option<Box<dyn Timer>>>> = Rc::default();
        let cancel = {
            let timer = timer.clone();
            move || {
                if let Some(t) = timer.borrow_mut().take() {
                    t.cancel();
                }
            }
        };
        let end: Rc<dyn Fn()> = {
            let w = self.me.clone();
            Rc::new(move || {
                let Some(e) = w.upgrade() else { return };
                // The `finally` of the connected case.
                if !e.status_is(ConnectionStatus::Connected)
                    && e.connection_ready.borrow().is(&ready)
                {
                    *e.connection_ready.borrow_mut() = ReadyPromise::default();
                }
            })
        };
        {
            let w = self.me.clone();
            let cancel = cancel.clone();
            let end = end.clone();
            *self.abort_ping_timeout.borrow_mut() = Rc::new(move || {
                cancel();
                let end = end.clone();
                if let Some(e) = w.upgrade() {
                    e.micro(move |e| {
                        if e.is_token(token) {
                            end();
                            e.iterate();
                        }
                    });
                }
            });
        }
        {
            let end = end.clone();
            let t = self.later(self.options.ping_timeout_ms, move |e| {
                if e.is_token(token) {
                    e.ping(end);
                }
            });
            *timer.borrow_mut() = Some(t);
        }
        {
            let w = self.me.clone();
            let cancel = cancel.clone();
            let end = end.clone();
            *self.ping_now.borrow_mut() = Rc::new(move || {
                if let Some(e) = w.upgrade()
                    && e.is_token(token)
                {
                    cancel();
                    e.ping(end.clone());
                }
            });
        }
        {
            let cancel = cancel.clone();
            let end = end.clone();
            self.on_visibility(false, token, move |e| {
                cancel();
                e.disconnect(
                    ConnectionError::client(
                        ClientErrorKind::Hidden,
                        "Connection closed because tab was hidden",
                    ),
                    None,
                );
                end();
                e.iterate();
            });
        }
        self.on_state_change(token, move |e, state| {
            end();
            e.resume_after_state_change(state);
        });
    }

    fn ping(&self, end: Rc<dyn Fn()>) {
        let token = self.next_loop_token();
        let t0 = self.scheduler.monotonic_now();
        // Before the send, as JS sets `#onPong` first: a transport may
        // deliver the pong from inside `send`.
        {
            let w = self.me.clone();
            let end = end.clone();
            *self.on_pong.borrow_mut() = Rc::new(move || {
                let end = end.clone();
                if let Some(e) = w.upgrade() {
                    e.micro(move |e| {
                        if !e.is_token(token) {
                            return;
                        }
                        let ms = e.scheduler.monotonic_now() - t0;
                        e.log
                            .debug(|| format!("ping succeeded in {} ms", number::format(ms)));
                        end();
                        e.iterate();
                    });
                }
            });
        }
        self.send(Upstream::Ping);
        {
            let end = end.clone();
            // The timer ends with the token; it needn't be cancelled.
            let _ = self.later(self.options.ping_timeout_ms, move |e| {
                if !e.is_token(token) {
                    return;
                }
                let ms = e.scheduler.monotonic_now() - t0;
                e.log
                    .info(|| format!("ping failed in {} ms - disconnecting", number::format(ms)));
                let error = ConnectionError::client(
                    ClientErrorKind::PingTimeout,
                    "Server ping request failed",
                );
                e.disconnect(error.clone(), None);
                end();
                e.loop_failed(error);
            });
        }
        self.on_state_change(token, move |e, state| {
            end();
            e.resume_after_state_change(state);
        });
    }

    /// needs-auth and error: wait for `connect()`.
    fn wait_for_connect_request(&self, token: u64) {
        let w = self.me.clone();
        self.connection_manager.on_connect_request(move || {
            if let Some(e) = w.upgrade()
                && e.is_token(token)
            {
                e.connection_manager.resume_from_connect_request();
                e.iterate();
            }
        });
        self.on_state_change(token, |e, _| e.iterate());
    }

    // MARK: The app's lifecycle

    /// The app was shown, or hidden. Hidden, the client disconnects and
    /// waits to be shown again.
    pub(crate) fn set_visible(&self, visible: bool) {
        if let Some(t) = self.hidden_timer.borrow_mut().take() {
            t.cancel();
        }
        if visible {
            self.set_visibility_state(true);
        } else {
            let t = self.later(self.options.hidden_disconnect_delay_ms, |e| {
                e.hidden_timer.borrow_mut().take();
                e.set_visibility_state(false);
            });
            *self.hidden_timer.borrow_mut() = Some(t);
        }
    }

    fn set_visibility_state(&self, visible: bool) {
        if visible == self.visible.get() {
            return;
        }
        self.visible.set(visible);
        let ready: Vec<VisibilityWaiter> = {
            let mut waiters = self.visibility_waiters.borrow_mut();
            let (ready, rest) = std::mem::take(&mut *waiters)
                .into_iter()
                .partition(|(v, _, _)| *v == visible);
            *waiters = rest;
            ready
        };
        for (_, _, w) in ready {
            w();
        }
    }

    /// Calls `body` in a microtask once the app's visibility is `visible`,
    /// unless the run loop's wait `token` is over by then.
    fn on_visibility(&self, visible: bool, token: u64, body: impl FnOnce(&Engine) + 'static) {
        let w = self.me.clone();
        let waiter: Box<dyn FnOnce()> = Box::new(move || {
            if let Some(e) = w.upgrade() {
                e.micro(move |e| {
                    if !e.is_token(token) {
                        return;
                    }
                    // The wait is over: a state change no longer ends it.
                    e.next_loop_token();
                    body(e);
                });
            }
        });
        if self.visible.get() == visible {
            waiter();
        } else {
            let current = self.loop_token.get();
            let mut waiters = self.visibility_waiters.borrow_mut();
            waiters.retain(|(_, t, _)| *t == current);
            waiters.push((visible, token, waiter));
        }
    }

    /// The network may be back: retry now instead of after the backoff,
    /// and check that an open connection still works.
    pub(crate) fn reconnect_now(&self) {
        let backoff = {
            let mut b = self.backoff.borrow_mut();
            match &*b {
                Some((token, _)) if *token == self.loop_token.get() => b.take(),
                _ => None,
            }
        };
        if let Some((_, timer)) = backoff {
            timer.cancel();
            self.iterate();
        } else if self.status_is(ConnectionStatus::Connected) {
            let ping_now = self.ping_now.borrow().clone();
            ping_now();
        }
    }

    fn pending_settled(&self) -> bool {
        self.store.borrow().pending.is_empty()
            || !self.status_is(ConnectionStatus::Connected)
            || self.client_group_disabled.get()
            || self.rep_closed.get()
    }

    /// Calls `done` once the server has confirmed every pending mutation,
    /// or right away when it can't: the client isn't connected, or the
    /// group is disabled.
    pub(crate) fn when_pending_settled(&self, done: impl FnOnce() + 'static) {
        if self.pending_settled() {
            done();
        } else {
            self.pending_waiters.borrow_mut().push(Box::new(done));
        }
    }

    fn settle_pending_waiters(&self) {
        if self.pending_waiters.borrow().is_empty() || !self.pending_settled() {
            return;
        }
        let waiters = std::mem::take(&mut *self.pending_waiters.borrow_mut());
        for w in waiters {
            w();
        }
    }

    // MARK: Connecting

    fn add_connect_event(&self, attempt: &ConnectAttempt, event: &str) {
        attempt
            .events
            .borrow_mut()
            .push((self.scheduler.now(), event.to_owned()));
    }

    fn arm_connect_timeout(&self, attempt: &Rc<ConnectAttempt>, ack: bool) {
        attempt.cancel_timeout();
        if attempt.aborted.get() {
            return;
        }
        let weak = Rc::downgrade(attempt);
        let t = self.later(CONNECT_TIMEOUT_MS, move |e| {
            let Some(attempt) = weak.upgrade() else {
                return;
            };
            let events = Value::Array(
                attempt
                    .events
                    .borrow()
                    .iter()
                    .map(|(t, ev)| {
                        Value::Array(vec![
                            Value::String(iso_date_string(*t)),
                            Value::String(ev.clone()),
                        ])
                    })
                    .collect(),
            );
            e.log.debug(|| "Connection attempt timed out".into());
            let seconds = number::format(CONNECT_TIMEOUT_MS / 1000.0);
            let message = format!(
                "{}Connect events: {}",
                if ack {
                    format!("Server did not acknowledge the connection within {seconds} seconds. ")
                } else {
                    format!("Connection setup timed out after {seconds} seconds. ")
                },
                json::stringify(&events)
            );
            e.disconnect(
                ConnectionError::client(ClientErrorKind::ConnectTimeout, message),
                None,
            );
        });
        *attempt.timeout.borrow_mut() = Some(t);
    }

    fn begin_attempt(&self) {
        assert!(
            self.current_attempt.borrow().is_none(),
            "Connection attempt already active"
        );
        let attempt = Rc::new(ConnectAttempt::default());
        *self.current_attempt.borrow_mut() = Some(attempt.clone());
        self.add_connect_event(&attempt, "starting the connection");
        self.arm_connect_timeout(&attempt, false);
        self.connect_attempt(&attempt);
    }

    fn token(&self) -> Option<String> {
        let auth = self.auth.borrow();
        (!auth.is_empty()).then(|| auth.clone())
    }

    /// `#connectAttempt`: opens a socket and waits for `connected`.
    fn connect_attempt(&self, attempt: &Rc<ConnectAttempt>) {
        if self.rep_closed.get() {
            self.finish_attempt(attempt, None);
            return;
        }
        let (Some(socket_origin), Some(socket_factory)) =
            (self.socket_origin.clone(), self.socket_factory.clone())
        else {
            self.finish_attempt(
                attempt,
                Some(ConnectionError::client(
                    ClientErrorKind::Internal,
                    "No server provided",
                )),
            );
            return;
        };
        assert!(
            self.status_is(ConnectionStatus::Disconnected)
                || self.status_is(ConnectionStatus::Connecting),
            "connectAttempt() called from invalid state"
        );
        let wsid = random_id();
        self.log.info(|| "Connecting...".into());
        self.connection_manager.connecting(None);
        attempt.start.set(self.scheduler.monotonic_now());
        self.add_connect_event(attempt, "reading the cookie");
        let connect_cookie = self.store.borrow().cookie().clone();
        self.add_connect_event(attempt, "reading the client group ID");
        self.add_connect_event(attempt, "initializing active clients");
        if attempt.aborted.get() {
            let reason = attempt.abort_reason.borrow().clone();
            self.finish_attempt(attempt, reason);
            return;
        }

        // `createSocket`.
        self.add_connect_event(attempt, "reading the profile ID");
        let url = ConnectionUrl {
            socket_origin,
            client_id: self.client_id.clone(),
            client_group_id: self.client_group_id.clone(),
            user_id: self.user_id.clone(),
            base_cookie: match &connect_cookie {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(number::format(*n)),
                _ => None,
            },
            timestamp: self.scheduler.monotonic_now(),
            last_mutation_id: self.last_mutation_id_received.get(),
            wsid,
            profile_id: self.profile_id.clone(),
            debug_perf: false,
            additional_params: self
                .additional_connect_params
                .borrow()
                .clone()
                .unwrap_or_default(),
        }
        .to_string();
        self.add_connect_event(attempt, "reading deleted clients");
        let mut deleted = self.deleted_clients_body();
        self.add_connect_event(attempt, "reading desired queries");
        let mut queries_patch = Some(
            self.query_manager
                .queries_patch(&self.store.borrow().main, None),
        );
        let mut active_clients = vec![self.client_id.clone()];
        active_clients.extend(
            self.database
                .active_clients(&self.client_group_id)
                .into_iter()
                .filter(|c| *c != self.client_id),
        );
        let init_connection = InitConnectionBody {
            desired_queries_patch: queries_patch
                .as_ref()
                .map(|p| p.values().cloned().collect())
                .unwrap_or_default(),
            client_schema: connect_cookie.is_null().then(|| self.client_schema.clone()),
            deleted: deleted.clone(),
            user_push_url: self.options.mutate_url.clone(),
            user_push_headers: self.options.mutate_headers.clone(),
            user_query_url: self.options.query_url.clone(),
            user_query_headers: self.options.query_headers.clone(),
            active_clients: Some(active_clients),
            traceparent: None,
        };
        let auth_token = self.token();
        let mut sec_protocol = sec_protocols::encode(Some(&init_connection), auth_token.as_deref());
        if sec_protocol.len() > self.options.max_header_length {
            sec_protocol = sec_protocols::encode(None, auth_token.as_deref());
            queries_patch = None;
            if sec_protocol.len() > self.options.max_header_length {
                let (len, max) = (sec_protocol.len(), self.options.max_header_length);
                self.log.warn(|| {
                    format!(
                        "Encoded auth token length ({len}) exceeds ZeroOptions.maxHeaderLength \
                         ({max}). This may cause connection failures."
                    )
                });
            }
        } else {
            deleted = None;
        }
        self.log.info(|| format!("Connecting to {url}"));
        self.add_connect_event(attempt, "creating the WebSocket");
        let handle = SocketHandle::new(self.me.clone());
        let connection = socket_factory.open(&url, Some(&sec_protocol), handle.clone());
        *handle.connection.borrow_mut() = Some(connection);

        if attempt.aborted.get() {
            handle.close(None);
            let reason = attempt.abort_reason.borrow().clone();
            self.finish_attempt(attempt, reason);
            return;
        }
        if self.rep_closed.get() {
            handle.close(None);
            self.finish_attempt(attempt, None);
            return;
        }
        *self.connect_cookie.borrow_mut() = connect_cookie;
        *self.init_connection_queries.borrow_mut() = queries_patch;
        *self.deleted_clients.borrow_mut() = deleted;
        *self.socket.borrow_mut() = Some(handle);
        self.add_connect_event(attempt, "waiting for the server acknowledgement");
        self.log
            .debug(|| "Waiting for connection to be acknowledged".into());
        self.arm_connect_timeout(attempt, true);
        let (w, weak_attempt) = (self.me.clone(), Rc::downgrade(attempt));
        *attempt.on_connected.borrow_mut() = Some(Box::new(move || {
            let (Some(e), Some(attempt)) = (w.upgrade(), weak_attempt.upgrade()) else {
                return;
            };
            if attempt.aborted.get() {
                let reason = attempt.abort_reason.borrow().clone();
                e.finish_attempt(&attempt, reason);
                return;
            }
            e.mutation_tracker
                .on_connected(e.last_mutation_id_received.get());
            e.request_push(true);
            e.finish_attempt(&attempt, None);
        }));
    }

    /// The run loop's handling of a finished attempt; `error` `None` for
    /// success.
    fn finish_attempt(&self, attempt: &Rc<ConnectAttempt>, error: Option<ConnectionError>) {
        if attempt.finished.replace(true) {
            return;
        }
        attempt.cancel_timeout();
        {
            let mut current = self.current_attempt.borrow_mut();
            if current.as_ref().is_some_and(|c| Rc::ptr_eq(c, attempt)) {
                *current = None;
            }
        }
        let ready = self.connection_ready.borrow().clone();
        if let Some(error) = error {
            if !attempt.aborted.get() {
                self.disconnect(error.clone(), None);
            }
            ready.settle(false, &self.event_loop);
            if self.connection_ready.borrow().is(&ready) {
                *self.connection_ready.borrow_mut() = ReadyPromise::default();
            }
            self.loop_failed(error);
            return;
        }
        ready.settle(true, &self.event_loop);
        *self.additional_connect_params.borrow_mut() = None;
        match Self::connection_error(&self.connection_manager.state()) {
            Some(error) => self.loop_failed(error),
            None => self.iterate(),
        }
    }

    /// `#disconnect`: closes the socket and moves the connection status.
    pub(crate) fn disconnect(&self, reason: ConnectionError, close_code: Option<u16>) {
        let attempt = self.current_attempt.borrow().clone();
        if let Some(attempt) = attempt
            && !attempt.aborted.get()
        {
            *attempt.abort_reason.borrow_mut() = Some(reason.clone());
            attempt.aborted.set(true);
            attempt.cancel_timeout();
            let reason = reason.clone();
            self.micro(move |e| e.finish_attempt(&attempt, Some(reason)));
        }
        let status = self.connection_manager.status();
        self.log.info(|| {
            format!(
                "disconnecting: reason={} connectionState={}",
                reason.kind_name(),
                status.as_str()
            )
        });
        if status == ConnectionStatus::Closed {
            self.log.debug(|| "disconnect() called while closed".into());
            return;
        }
        if let Some(at) = self.connected_at.get()
            && self.scheduler.monotonic_now() - at >= STABLE_CONNECTION_MS
        {
            self.transport_failures.set(0);
        }
        self.connected_at.set(None);
        let socket = self.socket.borrow_mut().take();
        if let Some(s) = socket {
            s.close(close_code);
        }
        self.last_mutation_ids_sent.borrow_mut().clear();
        self.poke_handler.borrow_mut().clear();
        self.query_manager.clear_got_queries_authoritative();
        let (transition, r) = ConnectionTransition::of(&reason);
        match transition {
            ConnectionTransition::NeedsAuth => self.connection_manager.needs_auth(r),
            ConnectionTransition::Error => self.connection_manager.error(r),
            ConnectionTransition::Disconnected => self.connection_manager.disconnected(r),
            ConnectionTransition::Closed => self.connection_manager.closed(),
            ConnectionTransition::None => {
                if !self.connection_manager.is_in_terminal_state() {
                    self.connection_manager.connecting(Some(r));
                }
            }
        }
    }

    // MARK: Messages

    fn is_socket(&self, handle: &Rc<SocketHandle>) -> bool {
        self.socket
            .borrow()
            .as_ref()
            .is_some_and(|s| Rc::ptr_eq(s, handle))
    }

    fn socket_did_receive(&self, handle: &Rc<SocketHandle>, message: SocketMessage) {
        if !self.is_socket(handle) {
            return;
        }
        let event_loop = self.event_loop.clone();
        event_loop.dispatch(|| self.on_message(message));
    }

    fn socket_did_open(&self, handle: &Rc<SocketHandle>) {
        if !self.is_socket(handle) {
            return;
        }
        let Some(attempt) = self.current_attempt.borrow().clone() else {
            return;
        };
        let ms = self.scheduler.monotonic_now() - attempt.start.get();
        self.log
            .info(|| format!("Got socket open event: timeToOpenMs={}", number::format(ms)));
    }

    fn socket_did_close(
        &self,
        handle: &Rc<SocketHandle>,
        code: u16,
        reason: &str,
        was_clean: bool,
    ) {
        if !self.is_socket(handle) {
            return;
        }
        let event_loop = self.event_loop.clone();
        event_loop.dispatch(|| {
            // 1005 and 1006 are how platforms report a close without a
            // frame.
            if code <= 1001 || code == 1005 || code == 1006 {
                self.log.info(|| {
                    format!("Got socket close event: code={code} reason={reason} wasClean={was_clean}")
                });
            } else {
                self.log.error(|| {
                    format!(
                        "Got unexpected socket close event: code={code} reason={reason} wasClean={was_clean}"
                    )
                });
            }
            if code == 1009 {
                let mut message = "A WebSocket message exceeded the server message size limit. \
                    This is usually caused by a mutation's args containing a large value such as \
                    a base64-encoded image. Consider uploading large files to object storage and \
                    storing only the URL in Zero."
                    .to_owned();
                let recent = self.recent_sent_messages.borrow();
                if !recent.is_empty() {
                    message.push_str("\nRecent sent messages:\n");
                    message.push_str(&pretty_recent_messages(&recent));
                }
                drop(recent);
                self.disconnect(
                    ConnectionError::client(ClientErrorKind::InvalidMessage, message),
                    None,
                );
                self.disable_client_group();
                self.micro(|e| e.client_state_not_found());
            } else if was_clean {
                self.disconnect(
                    ConnectionError::client(
                        ClientErrorKind::CleanClose,
                        "WebSocket connection closed cleanly",
                    ),
                    None,
                );
            } else {
                self.disconnect(
                    ConnectionError::client(
                        ClientErrorKind::AbruptClose,
                        "WebSocket connection closed abruptly",
                    ),
                    None,
                );
            }
        });
    }

    fn abort_ping_timeout(&self) {
        let f = self.abort_ping_timeout.borrow().clone();
        f();
    }

    /// Disconnects with `error` for a message that doesn't make sense.
    fn fail_on_message(&self, error: ConnectionError) {
        self.disconnect(error, None);
    }

    fn on_message(&self, message: SocketMessage) {
        if self.rep_closed.get() {
            return;
        }
        let text = match message {
            SocketMessage::Binary(bytes) => {
                if bytes.first() != Some(&poke_chunk::MESSAGE_TYPE) {
                    let t = bytes
                        .first()
                        .map_or_else(|| "undefined".to_owned(), |b| b.to_string());
                    self.fail_on_message(ConnectionError::client(
                        ClientErrorKind::InvalidMessage,
                        format!("Unknown binary message type: {t}"),
                    ));
                    return;
                }
                self.abort_ping_timeout();
                let result = self
                    .poke_handler
                    .borrow_mut()
                    .handle_poke_chunk(&bytes[1..]);
                if let Err(m) = result {
                    self.poke_message_error(m);
                }
                return;
            }
            SocketMessage::Text(t) => t,
        };
        let downstream = match Downstream::from_text(&text) {
            Ok(d) => d,
            Err(e) => {
                self.fail_on_message(ConnectionError::client(
                    ClientErrorKind::InvalidMessage,
                    format!(
                        "Invalid message received from server: {}{text}",
                        valita_message(&e, &text)
                    ),
                ));
                return;
            }
        };
        match downstream {
            Downstream::Connected(body) => self.micro(move |e| e.handle_connected(body)),
            Downstream::Error(body) => self.handle_error_message(body),
            Downstream::Pong => {
                let f = self.on_pong.borrow().clone();
                f();
            }
            Downstream::PokeStart(body) => {
                self.abort_ping_timeout();
                let result = self.poke_handler.borrow_mut().handle_poke_start(body);
                if let Err(m) = result {
                    self.poke_message_error(m);
                }
            }
            Downstream::PokePart(body) => {
                self.abort_ping_timeout();
                let result = self.poke_handler.borrow_mut().handle_poke_part(body);
                match result {
                    Ok(Some(lmid)) => self.last_mutation_id_received.set(lmid),
                    Ok(None) => {}
                    Err(m) => self.poke_message_error(m),
                }
            }
            Downstream::PokeEnd(body) => {
                self.abort_ping_timeout();
                let result = self.poke_handler.borrow_mut().handle_poke_end(body);
                match result {
                    Ok(Some(r)) => {
                        if let Some(lmid) = r.last_mutation_id_change_for_self {
                            self.last_mutation_id_received.set(lmid);
                        }
                        if !self.poke_handler.borrow().playback_running {
                            self.start_playback();
                        }
                    }
                    Ok(None) => {}
                    Err(m) => self.poke_message_error(m),
                }
            }
            Downstream::Pull(_) => self.abort_ping_timeout(),
            Downstream::DeleteClients(body) => self.database.confirm_deleted_clients(
                body.client_ids.as_deref().unwrap_or_default(),
                body.client_group_ids.as_deref().unwrap_or_default(),
            ),
            Downstream::PushResponse(body) => {
                if matches!(body, PushResponseBody::Error(_)) {
                    self.log.error(|| {
                        format!(
                            "Received an error response when pushing mutations: {}",
                            json::stringify(&body.to_json())
                        )
                    });
                }
                if let Err(e) = self.mutation_tracker.process_push_response(&body) {
                    self.disconnect(
                        ConnectionError::client(ClientErrorKind::Internal, e.message()),
                        None,
                    );
                }
            }
            Downstream::TransformError(errors) => {
                self.query_manager.handle_transform_errors(&errors)
            }
            Downstream::Inspect(_) => {}
        }
    }

    fn handle_connected(&self, _body: ConnectedBody) {
        let Some(attempt) = self.current_attempt.borrow().clone() else {
            return;
        };
        let ms = self.scheduler.monotonic_now() - attempt.start.get();
        self.log
            .info(|| format!("Connected: timeToConnectMs={}", number::format(ms)));
        self.add_connect_event(&attempt, "processing the server acknowledgement");
        self.connected_at.set(Some(self.scheduler.monotonic_now()));
        self.last_mutation_ids_sent.borrow_mut().clear();
        let init_queries = self.init_connection_queries.borrow_mut().take();
        let patch = self
            .query_manager
            .queries_patch(&self.store.borrow().main, init_queries.as_ref());
        let patch_ops: Vec<UpQueriesPatchOp> = patch.values().cloned().collect();
        match init_queries {
            Some(_) if !patch.is_empty() => {
                self.send(Upstream::ChangeDesiredQueries(ChangeDesiredQueriesBody {
                    desired_queries_patch: patch_ops,
                    traceparent: None,
                }));
            }
            Some(_) => {}
            None => {
                let deleted = self.deleted_clients.borrow_mut().take();
                let client_schema = self
                    .connect_cookie
                    .borrow()
                    .is_null()
                    .then(|| self.client_schema.clone());
                self.send(Upstream::InitConnection(InitConnectionBody {
                    desired_queries_patch: patch_ops,
                    client_schema,
                    deleted,
                    user_push_url: self.options.mutate_url.clone(),
                    user_push_headers: self.options.mutate_headers.clone(),
                    user_query_url: self.options.query_url.clone(),
                    user_query_headers: self.options.query_headers.clone(),
                    active_clients: None,
                    traceparent: None,
                }));
            }
        }
        let deleted = self.deleted_clients.borrow_mut().take();
        if let Some(deleted) = deleted {
            self.send(Upstream::DeleteClients(deleted));
        }
        self.connection_manager.connected();
        let on_connected = attempt.on_connected.borrow_mut().take();
        if let Some(f) = on_connected {
            self.event_loop.microtask(f);
        }
    }

    fn handle_error_message(&self, body: ErrorBody) {
        let kind = body.kind();
        if kind == ErrorKind::MutationRateLimited {
            self.last_mutation_ids_sent.borrow_mut().clear();
            self.log.error(|| {
                format!(
                    "{} Mutation rate limited: {}",
                    kind.as_str(),
                    body.message()
                )
            });
            return;
        }
        // Being sent elsewhere or later, a lost client group (which
        // recovers), and auth failures are routine; every other kind ends
        // the connection.
        let message = body.message().to_owned();
        let error = ConnectionError::Server(Box::new(body));
        let routine =
            error.backoff().is_some() || kind == ErrorKind::ClientNotFound || error.is_auth_error();
        self.log.log(
            if routine {
                LogLevel::Warn
            } else {
                LogLevel::Error
            },
            || format!("{}:\n\n{message}", kind.as_str()),
        );
        self.disconnect(error, None);
        match kind {
            ErrorKind::VersionNotSupported => self.update_needed(UpdateNeededReason {
                type_: kind.as_str().to_owned(),
                message: Some(message),
            }),
            ErrorKind::SchemaVersionNotSupported => {
                self.disable_client_group();
                self.micro(move |e| {
                    e.update_needed(UpdateNeededReason {
                        type_: "SchemaVersionNotSupported".into(),
                        message: Some(message),
                    })
                });
            }
            ErrorKind::ClientNotFound => {
                self.disable_client_group();
                self.micro(|e| e.client_state_not_found());
            }
            ErrorKind::InvalidConnectionRequestLastMutationID
            | ErrorKind::InvalidConnectionRequestBaseCookie => {
                // The server is behind the replica: drop it.
                self.database.drop_database();
                self.micro(|e| e.client_state_not_found());
            }
            _ => {}
        }
    }

    /// A poke message that doesn't fit the poke being received.
    fn poke_message_error(&self, message: String) {
        self.poke_handler.borrow_mut().clear();
        self.on_poke_error(&message);
    }

    fn on_poke_error(&self, message: &str) {
        if self.status_is(ConnectionStatus::Disconnected) {
            return;
        }
        if message.contains("unexpected base cookie") {
            self.log.debug(|| format!("clearing due to {message}"));
            self.disconnect(
                ConnectionError::client(
                    ClientErrorKind::UnexpectedBaseCookie,
                    "Server returned unexpected base cookie during sync",
                ),
                None,
            );
        } else {
            self.log
                .error(|| format!("clearing due to unexpected poke error: {message}"));
            self.disconnect(
                ConnectionError::client(
                    ClientErrorKind::Internal,
                    format!("Poke processing error: {message}"),
                ),
                None,
            );
        }
    }

    // MARK: Poke playback

    fn start_playback(&self) {
        self.poke_handler.borrow_mut().playback_running = true;
        let _ = self.later(0.0, |e| e.playback_frame());
    }

    /// Applies the pokes that arrived since the last frame, at most once
    /// per scheduler turn, merged (`#rafCallback`).
    fn playback_frame(&self) {
        if !self.poke_handler.borrow().has_buffered() {
            self.poke_handler.borrow_mut().playback_running = false;
            return;
        }
        let _ = self.later(0.0, |e| e.playback_frame());
        let merged = self.poke_handler.borrow_mut().take_merged();
        let merged = match merged {
            Ok(Some(m)) => m,
            Ok(None) => return,
            Err(e) => {
                self.poke_handler.borrow_mut().clear();
                self.on_poke_error(&e.0);
                return;
            }
        };
        let applied = self.with_write_lock(|| self.apply_poke(&merged));
        let result = applied.and_then(|()| {
            let lmid = merged
                .last_mutation_id_changes
                .get(&self.client_id)
                .copied();
            let advanced = match lmid {
                Some(lmid) => self
                    .mutation_tracker
                    .lmid_advanced(lmid)
                    .map_err(|m| format!("Error: {m}")),
                None => Ok(()),
            };
            self.settle_pending_waiters();
            advanced
        });
        if let Err(message) = result {
            self.poke_handler.borrow_mut().clear();
            self.on_poke_error(&message);
        }
    }

    // MARK: Auth

    pub(crate) fn set_auth(&self, auth: Option<String>) {
        // A token without a user was rejected at init.
        *self.auth.borrow_mut() = auth.clone().unwrap_or_default();
        if let Some(auth) = auth
            && !auth.is_empty()
        {
            self.send_if_connected(Upstream::UpdateAuth(UpdateAuthBody { auth }));
        }
    }

    /// `connection.connect()`: resumes from needs-auth or error. `auth`,
    /// when set, replaces the token first (`Some(None)` clears it).
    pub(crate) fn connect(&self, auth: Option<Option<String>>) {
        if let Some(auth) = auth {
            // JS's `#setAuth` throws this, so `connect` rejects before it
            // does anything.
            if let Err(e) = Self::check_auth_valid(self.user_id.as_deref(), auth.as_deref()) {
                self.log.error(|| format!("connect(auth:) ignored: {e}"));
                return;
            }
            self.set_auth(auth);
        }
        if let State::Disconnected(r) = self.connection_manager.state()
            && r.client_kind() == Some(ClientErrorKind::NoSocketOrigin)
        {
            self.log.error(|| {
                "connect() called but the connection is disconnected due to a missing cacheURL. \
                 No reconnect will be attempted."
                    .into()
            });
            return;
        }
        if !self.connection_manager.is_in_terminal_state() {
            return;
        }
        self.connection_manager.request_connect();
    }

    fn client_state_not_found(&self) {
        let f = self.on_client_state_not_found.borrow().clone();
        match f {
            Some(f) => f(),
            None => {
                if let Some(f) = &self.options.on_client_state_not_found {
                    f();
                }
            }
        }
    }

    fn update_needed(&self, reason: UpdateNeededReason) {
        let f = self.on_update_needed.borrow().clone();
        if let Some(f) = f {
            f(&reason);
        }
        if let Some(f) = &self.options.on_update_needed {
            f(&reason);
        }
    }

    // MARK: Close

    pub(crate) fn closed(&self) -> bool {
        self.rep_closed.get()
    }

    pub(crate) fn close(&self) {
        if self.rep_closed.get() {
            self.log
                .debug(|| "close() called on already closed instance".into());
            return;
        }
        self.log.debug(|| "Closing Zero instance".into());
        if !self.status_is(ConnectionStatus::Disconnected) {
            self.disconnect(
                ConnectionError::client(
                    ClientErrorKind::ClientClosed,
                    "Zero instance closed by user",
                ),
                Some(1000),
            );
        }
        self.rep_closed.set(true);
        if let Some(t) = self.push_timer.borrow_mut().take() {
            t.cancel();
        }
        // JS's close aborts a persist that hasn't run.
        if let Some(t) = self.persist_timer.borrow_mut().take() {
            t.cancel();
        }
        for t in std::mem::take(&mut *self.lifecycle_timers.borrow_mut()) {
            t.cancel();
        }
        self.database.client_stopped(&self.client_id);
        // Releasing writes what the store had queued; a failure to is
        // still this client's to report.
        self.database.release();
        self.database.remove_failure_listener(&self.client_id);
        self.connection_manager.closed();
    }

    // MARK: Storage and the client lifecycle

    /// Starts the heartbeat and the garbage collection of clients and
    /// client groups.
    fn start_lifecycle(&self) {
        let (h, c, g, d) = (
            self.me.clone(),
            self.me.clone(),
            self.me.clone(),
            self.me.clone(),
        );
        let timers = vec![
            self.scheduler.schedule_repeating(
                HEARTBEAT_INTERVAL_MS,
                Box::new(move || {
                    if let Some(e) = h.upgrade()
                        && !e.database.heartbeat(&e.client_id, e.scheduler.now())
                    {
                        e.client_state_not_found();
                    }
                }),
            ),
            self.scheduler.schedule_repeating(
                GC_INTERVAL_MS,
                Box::new(move || {
                    let Some(e) = c.upgrade() else { return };
                    if let Some(deleted) = e.database.collect_clients(
                        &e.client_id,
                        e.scheduler.now(),
                        CLIENT_MAX_INACTIVE_MS,
                    ) {
                        // `DeleteClientsManager.onClientsDeleted`.
                        let ids = deleted
                            .into_iter()
                            .filter(|p| p.client_group_id == e.client_group_id)
                            .map(|p| p.client_id)
                            .collect();
                        e.send_if_connected(Upstream::DeleteClients(DeleteClientsBody {
                            client_ids: Some(ids),
                            client_group_ids: None,
                        }));
                    }
                }),
            ),
            self.scheduler.schedule_repeating(
                GC_INTERVAL_MS,
                Box::new(move || {
                    if let Some(e) = g.upgrade() {
                        e.database.collect_client_groups();
                    }
                }),
            ),
            self.scheduler.schedule(
                INITIAL_COLLECT_DATABASES_DELAY_MS,
                Box::new(move || {
                    let Some(e) = d.upgrade() else { return };
                    e.collect_databases();
                    let w = e.me.clone();
                    let t = e.scheduler.schedule_repeating(
                        COLLECT_DATABASES_INTERVAL_MS,
                        Box::new(move || {
                            if let Some(e) = w.upgrade() {
                                e.collect_databases();
                            }
                        }),
                    );
                    e.lifecycle_timers.borrow_mut().push(t);
                }),
            ),
        ];
        *self.lifecycle_timers.borrow_mut() = timers;
    }

    /// Deletes the other databases of the store that no client has used
    /// for twice the time a client may be inactive (`collectIDBDatabases`),
    /// such as those of an older schema.
    fn collect_databases(&self) {
        self.database
            .collect_databases(self.scheduler.now(), 2.0 * CLIENT_MAX_INACTIVE_MS);
    }

    /// The store failed: the client keeps running on what it has in
    /// memory, and tells the app once.
    fn storage_failed(&self, f: &StorageFailure, opening: bool) {
        if self.storage_failure_reported.replace(true) {
            return;
        }
        if !opening {
            // A full or failing disk is the device's condition, not a
            // defect.
            self.log.warn(|| {
                format!(
                    "Local store storage failed ({}); persistence is stopped for this instance: {}",
                    f.kind.as_str(),
                    f.message
                )
            });
        }
        if let Some(cb) = &self.options.on_storage_failure {
            cb(f);
        }
    }

    /// The deleted clients of this client group, for `initConnection`
    /// (`convertDeletedClientsToBody`).
    fn deleted_clients_body(&self) -> Option<DeleteClientsBody> {
        let ids: Vec<String> = self
            .database
            .deleted_clients()
            .into_iter()
            .filter(|p| p.client_group_id == self.client_group_id)
            .map(|p| p.client_id)
            .collect();
        (!ids.is_empty()).then_some(DeleteClientsBody {
            client_ids: Some(ids),
            client_group_ids: None,
        })
    }

    /// The server lost the client group: stop pushing, and keep later
    /// clients out of it.
    fn disable_client_group(&self) {
        self.client_group_disabled.set(true);
        self.database.disable_client_group(&self.client_group_id);
    }

    // MARK: Mutations

    /// `MutatorProxy#onConnectionStateChange`.
    fn on_connection_state_change(&self, state: &State) {
        match state {
            State::Disconnected(r) if r.client_kind() == Some(ClientErrorKind::NoSocketOrigin) => {
                *self.mutation_rejection.borrow_mut() = None;
            }
            State::Disconnected(r) | State::Error(r) | State::Closed(r) => {
                *self.mutation_rejection.borrow_mut() = Some(r.clone());
                self.mutation_tracker.reject_all_outstanding_mutations(r);
            }
            _ => *self.mutation_rejection.borrow_mut() = None,
        }
        self.settle_pending_waiters();
    }

    /// What a mutation's caller hears for an error (`wrapErrorFor`).
    fn outcome(error: &Error) -> MutationOutcome {
        if let Error::Connection(e) = error
            && e.is_zero_error()
        {
            return MutationOutcome::ZeroError {
                message: e.message().to_owned(),
            };
        }
        let app = ApplicationError::wrapping(error);
        MutationOutcome::AppError {
            message: app.message,
            details: app.details,
        }
    }

    fn log_failure(&self, name: &str, outcome: &MutationOutcome, origin: &str) {
        match outcome {
            MutationOutcome::ZeroError { message } => self
                .log
                .error(|| format!("Mutator \"{name}\" error on {origin}: {message}")),
            MutationOutcome::AppError { message, .. } => self
                .log
                .error(|| format!("Mutator \"{name}\" app error on {origin}: {message}")),
            MutationOutcome::Success { .. } => {}
        }
    }

    /// `zero.mutate`, through the `MutatorProxy` and Replicache's `mutate`.
    ///
    /// A mutation made while listeners are notified, from a listener, runs
    /// once the current task is done, as an async JS mutator commits after
    /// the notification: every listener sees the change that notified it
    /// before the mutation's, and every view sees both.
    pub(crate) fn mutate(&self, name: &str, args: Option<Value>) -> MutatorResult {
        let client = Deferred::new();
        let server = Deferred::new();
        let result = MutatorResult {
            client: client.clone(),
            server: server.clone(),
        };
        if self.context.is_notifying() {
            let w = self.me.clone();
            let name = name.to_owned();
            self.event_loop.after_current_task(move || {
                let Some(e) = w.upgrade() else { return };
                let r = e.mutate(&name, args);
                r.client.on_settle(move |o| client.settle(o.clone()));
                r.server.on_settle(move |o| server.settle(o.clone()));
            });
            return result;
        }
        // JS throws this before it looks at the connection; here both
        // outcomes carry it, and nothing is written or pushed.
        let Some(mutator) = self.mutators.get(name).cloned() else {
            let message = format!(
                "Mutator \"{name}\" is not registered. Mutators must be registered with the Zero \
                 constructor before use."
            );
            self.log.error(|| message.clone());
            client.resolve(MutationOutcome::ZeroError {
                message: message.clone(),
            });
            server.resolve(MutationOutcome::ZeroError { message });
            return result;
        };
        let rejection = self.mutation_rejection.borrow().clone();
        if let Some(rejection) = rejection {
            self.log.warn(|| {
                format!(
                    "Mutation \"{name}\" rejected because Zero is {}. Details: {}.",
                    rejection.state_description(),
                    rejection.message()
                )
            });
            let message = rejection.message().to_owned();
            client.resolve(MutationOutcome::ZeroError {
                message: message.clone(),
            });
            server.resolve(MutationOutcome::ZeroError { message });
            return result;
        }
        let (ephemeral_id, tracked) = self.mutation_tracker.track_mutation();
        {
            let w = self.me.clone();
            let name = name.to_owned();
            let server = server.clone();
            tracked.on_settle(move |r| match r {
                Ok(data) => server.resolve(MutationOutcome::Success { data: data.clone() }),
                Err(error) => {
                    let outcome = Self::outcome(error);
                    if let Some(e) = w.upgrade() {
                        e.log_failure(&name, &outcome, "server");
                    }
                    server.resolve(outcome);
                }
            });
        }
        // A mutate from inside a mutator, or while a poke rebases, waits for
        // it, as JS's waits for Replicache's write lock.
        if self.write_locked.get() || !self.waiting_for_write_lock.borrow().is_empty() {
            let w = self.me.clone();
            let name = name.to_owned();
            self.waiting_for_write_lock
                .borrow_mut()
                .push_back(Box::new(move || {
                    if let Some(e) = w.upgrade() {
                        e.with_write_lock(|| {
                            e.run_mutation(&mutator, &name, args, ephemeral_id, &client)
                        });
                    }
                }));
        } else {
            self.with_write_lock(|| self.run_mutation(&mutator, name, args, ephemeral_id, &client));
        }
        result
    }

    fn with_write_lock<T>(&self, body: impl FnOnce() -> T) -> T {
        self.write_locked.set(true);
        let result = body();
        self.write_locked.set(false);
        if !self.waiting_for_write_lock.borrow().is_empty() {
            self.micro(|e| {
                if e.write_locked.get() {
                    return;
                }
                let next = e.waiting_for_write_lock.borrow_mut().pop_front();
                if let Some(next) = next {
                    next();
                }
            });
        }
        result
    }

    fn run_mutation(
        &self,
        mutator: &MutatorFn,
        name: &str,
        args: Option<Value>,
        ephemeral_id: u64,
        client: &Deferred<MutationOutcome>,
    ) {
        let mutation_id = self.store.borrow().next_mutation_id(&self.client_id);
        // Last mutation IDs are checked when they arrive, so this holds.
        if !number::is_safe_integer(mutation_id) {
            let error: Error = ConnectionError::client(
                ClientErrorKind::Internal,
                format!("Invalid mutation ID {}", number::format(mutation_id)),
            )
            .into();
            self.mutation_tracker.reject_mutation(ephemeral_id, &error);
            client.resolve(Self::outcome(&error));
            return;
        }
        let write = KvWrite::new(self.store.borrow().main.clone());
        let branch = self.ivm_main.fork();
        self.mutation_tracker
            .mutation_id_assigned(ephemeral_id, mutation_id);
        let tx = Transaction::new(
            self.client_id.clone(),
            mutation_id,
            Reason::Optimistic,
            self.token(),
            write,
            branch,
            self.schema.clone(),
        );
        // JS forks the views' branch for the transaction, which throws once
        // that branch has failed.
        let ran = match self.context.branch_failure() {
            Some(f) => Err(f.into()),
            None => mutator(&tx, args.as_ref()),
        };
        let data = match ran {
            Ok(data) => data,
            Err(error) => {
                self.mutation_tracker.reject_mutation(ephemeral_id, &error);
                let outcome = Self::outcome(&error);
                self.log_failure(name, &outcome, "client");
                client.resolve(outcome);
                return;
            }
        };
        let (write, rows) = tx.finish();
        let pending = PendingMutation {
            id: mutation_id,
            client_id: self.client_id.clone(),
            name: name.to_owned(),
            args: args.clone().unwrap_or(Value::Null),
            timestamp: self.scheduler.monotonic_now(),
            writes: write.writes.clone(),
        };
        let stored = StoredMutation {
            client_id: self.client_id.clone(),
            id: mutation_id,
            name: name.to_owned(),
            args: pending.args.clone(),
            timestamp: pending.timestamp,
            aggregate_writes: aggregate_writes(&write),
        };
        let diff = self.store.borrow_mut().commit_mutation(pending, &write);
        self.database.apply(vec![StorageOp::AddMutation {
            group: self.client_group_id.clone(),
            mutation: stored,
        }]);
        if let Err(error) = self.context.process_changes(&diff, &rows) {
            // As in JS, the mutation stays committed and pending, but isn't
            // pushed now, and its client outcome is the error.
            self.fire_watchers(&diff, false);
            let outcome = Self::outcome(&error.into());
            self.log_failure(name, &outcome, "client");
            client.resolve(outcome);
            return;
        }
        self.request_push(false);
        self.fire_watchers(&diff, false);
        self.schedule_persist();
        client.resolve(MutationOutcome::Success { data });
    }

    /// The store watches: the mutation tracker on `m/<clientID>/`, then the
    /// query manager on `g/`. A watch hears only about diffs that touch its
    /// prefix, except for its first run.
    fn fire_watchers(&self, diff: &KvDiff, initial: bool) {
        let mutations = with_prefix(diff, &format!("{MUTATIONS_PREFIX}{}/", self.client_id));
        if initial || !mutations.is_empty() {
            // A fatal error disconnects from inside the tracker.
            let _ = self.mutation_tracker.process_mutation_responses(&mutations);
        }
        let got = with_prefix(diff, GOT_QUERIES_PREFIX);
        if initial || !got.is_empty() {
            self.query_manager.process_got_queries(&got);
        }
    }

    // MARK: Pushing

    /// `pushConnectionLoop.send(now)`.
    fn request_push(&self, now: bool) {
        if self.rep_closed.get() {
            return;
        }
        self.push_requested.set(true);
        if self.push_in_flight.get() {
            return;
        }
        if now {
            if let Some(t) = self.push_timer.borrow_mut().take() {
                t.cancel();
            }
            self.micro(|e| e.invoke_push());
        } else if self.push_timer.borrow().is_none() {
            let t = self.later(0.0, |e| {
                e.push_timer.borrow_mut().take();
                e.invoke_push();
            });
            *self.push_timer.borrow_mut() = Some(t);
        }
    }

    /// `#invokePush`: pushes the pending mutations once connected.
    fn invoke_push(&self) {
        if self.push_in_flight.get()
            || !self.push_requested.get()
            || self.rep_closed.get()
            || self.client_group_disabled.get()
        {
            return;
        }
        self.push_requested.set(false);
        let pending = self.store.borrow().pending.clone();
        if pending.is_empty() {
            return;
        }
        self.push_in_flight.set(true);
        let w = self.me.clone();
        let ready = self.connection_ready.borrow().clone();
        ready.on_settle(&self.event_loop, move |connected| {
            let Some(e) = w.upgrade() else { return };
            if connected {
                e.pusher(&pending);
            }
            e.push_in_flight.set(false);
            if !connected {
                e.push_requested.set(true);
            }
            if e.push_requested.get() {
                e.request_push(false);
            }
        });
    }

    /// `#pusher`: one `push` message per mutation not yet sent on this
    /// connection.
    fn pusher(&self, mutations: &[PendingMutation]) {
        let now = self.scheduler.now();
        let counter = self.push_request_counter.get() + 1;
        self.push_request_counter.set(counter);
        let request_id = format!("{}-{counter}", self.client_id);
        for m in mutations {
            let sent = self
                .last_mutation_ids_sent
                .borrow()
                .get(&m.client_id)
                .copied()
                .unwrap_or(0.0);
            if m.id <= sent {
                continue;
            }
            let timestamp = now - (self.scheduler.monotonic_now() - m.timestamp).round();
            self.send(Upstream::Push(PushBody {
                client_group_id: self.client_group_id.clone(),
                mutations: vec![Mutation::Custom {
                    id: m.id,
                    client_id: m.client_id.clone(),
                    name: m.name.clone(),
                    args: vec![m.args.clone()],
                    timestamp,
                }],
                push_version: 1.0,
                schema_version: None,
                timestamp: now,
                request_id: request_id.clone(),
                auth: None,
                traceparent: None,
            }));
            self.last_mutation_ids_sent
                .borrow_mut()
                .insert(m.client_id.clone(), m.id);
        }
    }

    // MARK: Pokes

    /// `rep.poke`: applies a merged poke to the snapshot and rebases the
    /// pending mutations on it (`handlePullResponseV1` and
    /// `maybeEndPull`). Errors are JS's `String(error)`.
    fn apply_poke(&self, poke: &MergedPoke) -> Result<(), String> {
        let socket_at_start = self.socket.borrow().clone();
        let base = self.store.borrow().snapshot.clone();
        if poke.base_cookie != base.cookie {
            return Err(format!(
                "Error: unexpected base cookie for poke: {}",
                json::stringify(&poke.to_json())
            ));
        }
        let mut changes: Vec<(&String, &f64)> = poke.last_mutation_id_changes.iter().collect();
        changes.sort_by(|a, b| strings::utf16(a.0, b.0));
        for (cid, lmid) in changes {
            if let Some(last) = base.last_mutation_ids.get(cid)
                && lmid < last
            {
                return Err(format!(
                    "Error: {}",
                    bad_order_message(
                        &format!("{cid} lastMutationID"),
                        &number::format(*lmid),
                        &number::format(*last)
                    )
                ));
            }
        }
        if compare_cookies(&poke.cookie, &base.cookie) < 0.0 {
            return Err(format!(
                "Error: {}",
                bad_order_message(
                    "cookie",
                    &json::stringify(&poke.cookie),
                    &json::stringify(&base.cookie)
                )
            ));
        }
        if poke.cookie == base.cookie {
            // `zero.ts` trusts the got queries once `rep.poke` returns, even
            // for a poke that changes nothing.
            self.mark_got_queries_authoritative(socket_at_start.as_ref());
            return Ok(());
        }

        let mut snapshot = base;
        for (cid, lmid) in &poke.last_mutation_id_changes {
            snapshot.last_mutation_ids.insert(cid.clone(), *lmid);
        }
        snapshot.cookie = poke.cookie.clone();
        let mut patch_write = KvWrite::new(snapshot.map.clone());
        apply_patch(&poke.patch, &mut patch_write).map_err(|e| format!("Error: {}", e.0))?;
        self.check_updated_rows(&poke.patch, &patch_write)?;
        snapshot.map = patch_write.map.clone();

        let (store_main, pending_writes, pending) = {
            let store = self.store.borrow();
            (
                store.main.clone(),
                store.pending_writes(),
                store.pending.clone(),
            )
        };
        let mut touched: HashSet<String> =
            patch_write.writes.union(&pending_writes).cloned().collect();
        let mut main = snapshot.map.clone();
        let mut replayed = Vec::new();
        let pending: Vec<PendingMutation> = pending
            .into_iter()
            .filter(|m| {
                m.id > snapshot
                    .last_mutation_ids
                    .get(&m.client_id)
                    .copied()
                    .unwrap_or(0.0)
            })
            .collect();
        if !pending.is_empty() {
            // The rebase runs over a fork of the views' branch moved to the
            // new snapshot.
            let branch = self.ivm_main.fork();
            let moved = diff_keys(&store_main, &snapshot.map, &touched);
            capture(|| branch.apply(&moved, &HashMap::new())).map_err(|e| format!("Error: {e}"))?;
            let (m, r, t, _) =
                self.replay(&pending, &main, branch, &HashMap::new(), &mut |_, _| {});
            main = m;
            replayed = r;
            touched.extend(t);
        }
        let cookie = snapshot.cookie.clone();
        let last_mutation_ids = snapshot.last_mutation_ids.clone();
        let changes: Vec<(String, Option<Value>)> = {
            let mut keys: Vec<&String> = patch_write.writes.iter().collect();
            keys.sort();
            keys.into_iter()
                .filter_map(|k| {
                    let v = snapshot.map.get(k).cloned();
                    // After a clear, only what the poke put back.
                    (!(patch_write.cleared && v.is_none())).then(|| (k.clone(), v))
                })
                .collect()
        };
        let diff = self
            .store
            .borrow_mut()
            .advance(snapshot, main, replayed, Some(&touched));
        self.database.apply(vec![StorageOp::UpdateSnapshot {
            group: self.client_group_id.clone(),
            changes,
            cleared: patch_write.cleared,
            cookie,
            last_mutation_ids,
        }]);
        // A push that fails is a poke error: the client disconnects.
        self.context
            .process_changes(&diff, &HashMap::new())
            .map_err(|e| format!("Error: {e}"))?;
        self.fire_watchers(&diff, false);
        self.schedule_persist();
        self.mark_got_queries_authoritative(socket_at_start.as_ref());
        Ok(())
    }

    /// The server has caught the client up, unless the connection the poke
    /// came on went away while it was applied: the disconnect already
    /// re-gated trust, and the next connection's first poke restores it.
    fn mark_got_queries_authoritative(&self, socket_at_start: Option<&Rc<SocketHandle>>) {
        if let Some(s) = socket_at_start
            && self.is_socket(s)
        {
            self.query_manager.mark_got_queries_authoritative();
        }
    }

    /// Runs `pending` again, in order, over `map` and on forks of `branch`
    /// (the rebase of `maybeEndPull`). A mutation that fails stays pending
    /// without writes. Returns the new main map, the mutations with what
    /// they wrote, every key they wrote, and the branch they ran on.
    ///
    /// At launch, no query has registered the synced aggregates a mutation
    /// moves yet, so each run puts back the aggregate rows `restoring` has
    /// for the mutation, as JS's stored commit holds them. `ran` hears each
    /// mutation with its write, `None` when it failed.
    #[allow(clippy::type_complexity)]
    fn replay(
        &self,
        pending: &[PendingMutation],
        map: &KvMap,
        branch: Rc<IvmBranch>,
        restoring: &HashMap<MutationKey, AggregateWrites>,
        ran: &mut dyn FnMut(&PendingMutation, Option<&KvWrite>),
    ) -> (KvMap, Vec<PendingMutation>, HashSet<String>, Rc<IvmBranch>) {
        let mut main = map.clone();
        let mut branch = branch;
        let mut touched = HashSet::new();
        let mut replayed = Vec::new();
        for m in pending {
            let mut m = m.clone();
            let fork = branch.fork();
            let tx = Transaction::new(
                m.client_id.clone(),
                m.id,
                Reason::Rebase,
                self.token(),
                KvWrite::new(main.clone()),
                fork.clone(),
                self.schema.clone(),
            );
            let result = match self.mutators.get(&m.name) {
                Some(mutator) => mutator(&tx, Some(&m.args).filter(|a| !a.is_null())),
                None => Err(ApplicationError::new(format!("Unknown mutator: {}", m.name)).into()),
            };
            match result {
                Ok(_) => {
                    let (mut write, _) = tx.finish();
                    if let Some(writes) = restoring.get(&mutation_key(&m.client_id, m.id)) {
                        for (key, value) in writes {
                            match value {
                                Some(v) => write.set(key, v.clone()),
                                None => {
                                    write.delete(key);
                                }
                            }
                        }
                    }
                    main = write.map.clone();
                    branch = fork;
                    m.writes = write.writes.clone();
                    touched.extend(write.writes.iter().cloned());
                    ran(&m, Some(&write));
                }
                Err(_) => {
                    // The mutation stays pending; its writes are dropped.
                    m.writes = HashSet::new();
                    ran(&m, None);
                }
            }
            replayed.push(m);
        }
        (main, replayed, touched, branch)
    }

    /// Replicache's persist scheduler (`ProcessScheduler`): after a
    /// mutation or a poke, a persist runs once the last one is
    /// `PERSIST_THROTTLE_MS` old, then after `PERSIST_IDLE_TIMEOUT_MS`;
    /// requests while one waits join it.
    fn schedule_persist(&self) {
        if self.persist_timer.borrow().is_some() || self.rep_closed.get() {
            return;
        }
        let now = self.scheduler.monotonic_now();
        let throttled = self
            .last_persist_at
            .get()
            .map_or(0.0, |at| (at + PERSIST_THROTTLE_MS - now).max(0.0));
        let t = self.later(throttled + PERSIST_IDLE_TIMEOUT_MS, |e| e.persist());
        *self.persist_timer.borrow_mut() = Some(t);
    }

    /// `persistDD31`, for the synced aggregate rows the store keeps of each
    /// pending mutation. The snapshot and the mutations are written through
    /// as they come, and the mutations replayed at launch; JS persists its
    /// commits now and then, running the mutations again for it, so the
    /// aggregates they move are the ones registered at that persist, not
    /// when they ran. A persist runs the mutations it hasn't persisted over
    /// what it persisted before, or, when the snapshot is newer, all of
    /// them over it, and stores the aggregate rows each wrote.
    fn persist(&self) {
        self.persist_timer.borrow_mut().take();
        self.last_persist_at
            .set(Some(self.scheduler.monotonic_now()));
        let (pending, snapshot_cookie, snapshot_map, store_main, pending_writes) = {
            let store = self.store.borrow();
            (
                store.pending.clone(),
                store.snapshot.cookie.clone(),
                store.snapshot.map.clone(),
                store.main.clone(),
                store.pending_writes(),
            )
        };
        let to_run: Vec<PendingMutation> = {
            let mut persisted = self.persisted.borrow_mut();
            let to_run = if persisted.cookie != snapshot_cookie {
                persisted.cookie = snapshot_cookie;
                persisted.main = snapshot_map;
                persisted.touched.clear();
                pending.clone()
            } else {
                pending
                    .iter()
                    .filter(|m| {
                        !persisted
                            .mutations
                            .contains(&mutation_key(&m.client_id, m.id))
                    })
                    .cloned()
                    .collect()
            };
            persisted.mutations = pending
                .iter()
                .map(|m| mutation_key(&m.client_id, m.id))
                .collect();
            to_run
        };
        if to_run.is_empty() {
            return;
        }
        // The persist runs over a fork of the views' branch moved to what
        // it persisted.
        let (persisted_main, persisted_touched) = {
            let p = self.persisted.borrow();
            (p.main.clone(), p.touched.clone())
        };
        let branch = self.ivm_main.fork();
        let keys: HashSet<String> = pending_writes.union(&persisted_touched).cloned().collect();
        let moved = diff_keys(&store_main, &persisted_main, &keys);
        if capture(|| branch.apply(&moved, &HashMap::new())).is_err() {
            return;
        }
        let mut ops = Vec::new();
        let group = self.client_group_id.clone();
        let (main, _, touched, _) = self.replay(
            &to_run,
            &persisted_main,
            branch,
            &HashMap::new(),
            &mut |m, write| {
                ops.push(StorageOp::SetAggregateWrites {
                    group: group.clone(),
                    client_id: m.client_id.clone(),
                    id: m.id,
                    writes: write.map(aggregate_writes).unwrap_or_default(),
                });
            },
        );
        {
            let mut p = self.persisted.borrow_mut();
            p.main = main;
            p.touched.extend(touched);
        }
        self.database.apply(ops);
    }

    /// Rejects a poke whose `update` ops leave a row under a key that isn't
    /// its primary key: one whose merge drops or changes the key's columns.
    /// The table's source would then hold rows that disagree with the
    /// replica, which JS's `MemorySource` reports by throwing ("Row already
    /// exists", "Row not found") and the poke handler by disconnecting.
    fn check_updated_rows(&self, patch: &[KvPatchOp], write: &KvWrite) -> Result<(), String> {
        let mut keyless: HashMap<String, Vec<Row>> = HashMap::new();
        for op in patch {
            let KvPatchOp::Update { key, .. } = op else {
                continue;
            };
            let Some(value) = write.get(key) else {
                continue;
            };
            let table = keys::source_name(key);
            let Some(t) = self.schema.tables.get(&table) else {
                continue;
            };
            let row = value.as_object().cloned().unwrap_or_default();
            if let Ok(row_key) = keys::primary_key_string(&table, &t.primary_key, &row) {
                if row_key != *key {
                    return Err(format!(
                        "Error: update of {key} changes its primary key to {row_key}"
                    ));
                }
                continue;
            }
            // JS keeps a row whose merge leaves it without a usable primary
            // key, and fails once a second one collides with it in the
            // source.
            let Some(source) = self.ivm_main.source(&table) else {
                continue;
            };
            let row: Row = Rc::new(row);
            let previous = self
                .store
                .borrow()
                .main
                .get(key)
                .map(|v| Rc::new(v.as_object().cloned().unwrap_or_default()));
            let replaces_itself = previous.is_some_and(|p| source.same_key(&p, &row));
            let rows = keyless.entry(table).or_default();
            if (!replaces_itself && source.contains(&row))
                || rows.iter().any(|r| source.same_key(r, &row))
            {
                return Err(format!("Error: Row already exists: update of {key}"));
            }
            rows.push(row);
        }
        Ok(())
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.event_loop.stop();
        for t in self.lifecycle_timers.get_mut().drain(..) {
            t.cancel();
        }
        if !self.rep_closed.get() {
            self.database.client_stopped(&self.client_id);
            self.database.remove_failure_listener(&self.client_id);
            self.database.release();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_dates() {
        assert_eq!(
            iso_date_string(1_700_000_000_000.0),
            "2023-11-14T22:13:20.000Z"
        );
        assert_eq!(iso_date_string(0.0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_date_string(-1.0), "1969-12-31T23:59:59.999Z");
    }

    #[test]
    fn backoff() {
        assert_eq!(reconnect_backoff_ms(1, 5000.0, 0.0), 250.0);
        assert_eq!(reconnect_backoff_ms(1, 5000.0, 1.0), 500.0);
        assert_eq!(reconnect_backoff_ms(4, 5000.0, 0.5), 3000.0);
        assert_eq!(reconnect_backoff_ms(5, 5000.0, 0.5), 5000.0);
    }
}
