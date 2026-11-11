//! The public client (`Zero` in `packages/zero-client/src/client/zero.ts`)
//! and its connection (`connection.ts`).

use super::connection_manager::ConnectionState;
use super::database::{ReplicaDatabase, replicache_name};
use super::engine::{Engine, MutatorFn, MutatorResult, UpdateNeededReason, ZeroOptions};
use super::query_context::{MaterializedView, Preload, RunType};
use super::storage::DroppedDatabases;
use super::transaction::Transaction;
use super::{ApplicationError, Deferred, Error, Scheduler, SocketFactory};
use crate::ivm::ViewValue;
use crate::json::Value;
use crate::query::AnyQuery;
use crate::schema::Schema;
use crate::ttl::Ttl;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

type MutatorBody<C> = dyn Fn(&Transaction, Option<&Value>, &C) -> Result<Option<Value>, Error>;

/// A mutator the client can run and replay by name (`defineMutator` in
/// `packages/zql/src/mutate/mutator.ts`). The name must match the TS
/// registry entry the API server runs.
///
/// The body runs synchronously: once optimistically, and again on every
/// rebase over new server data while the mutation is pending, so it must
/// only depend on the transaction, the arguments, and the context. It
/// returns what the caller's `client` outcome carries.
pub struct Mutator<C> {
    pub name: String,
    body: Rc<MutatorBody<C>>,
}

impl<C> Clone for Mutator<C> {
    fn clone(&self) -> Self {
        Mutator {
            name: self.name.clone(),
            body: self.body.clone(),
        }
    }
}

impl<C> Mutator<C> {
    /// A mutator over JSON arguments: `args` is `None` when the caller
    /// passed none (JS `undefined`).
    pub fn new(
        name: impl Into<String>,
        body: impl Fn(&Transaction, Option<&Value>, &C) -> Result<Option<Value>, Error> + 'static,
    ) -> Self {
        Mutator {
            name: name.into(),
            body: Rc::new(body),
        }
    }
}

/// A Zero client: syncs the rows of named queries from zero-cache into a
/// local replica, keeps materialized views of them current, and runs
/// mutators optimistically before the server confirms them.
///
/// `C` is what mutators are given besides their arguments, such as the
/// logged-in user's auth data. Everything runs on the thread that made the
/// client, driven by the [`Scheduler`] and the socket events.
pub struct Zero<C = ()> {
    engine: Rc<Engine>,
    context: Rc<C>,
    connection: Connection,
}

impl<C: 'static> Zero<C> {
    /// - `schema`: the app's schema.
    /// - `mutators`: every mutator the client may run. Pending mutations
    ///   are replayed by name, so each must be registered.
    /// - `scheduler`: the clock and timers, on the app's event loop; tests
    ///   pass a [`ManualScheduler`](crate::testing::ManualScheduler).
    /// - `socket_factory`: opens the sync socket. Without one the client
    ///   runs locally.
    pub fn new(
        schema: Arc<Schema>,
        context: C,
        mutators: Vec<Mutator<C>>,
        options: ZeroOptions,
        scheduler: Rc<dyn Scheduler>,
        socket_factory: Option<Rc<dyn SocketFactory>>,
    ) -> Result<Self, Error> {
        let context = Rc::new(context);
        let mut functions: HashMap<String, MutatorFn> = HashMap::new();
        for m in mutators {
            if functions.contains_key(&m.name) {
                return Err(ApplicationError::new(format!(
                    "Mutator \"{}\" is registered twice",
                    m.name
                ))
                .into());
            }
            let (body, context) = (m.body.clone(), context.clone());
            functions.insert(m.name, Rc::new(move |tx, args| body(tx, args, &context)));
        }
        let engine = Engine::new(
            schema,
            options,
            Rc::new(functions),
            scheduler,
            socket_factory,
            true,
        )?;
        let connection = Connection::new(engine.clone());
        {
            let inner = Rc::downgrade(&connection.0);
            *engine.on_update_needed.borrow_mut() = Some(Rc::new(move |reason| {
                if let Some(c) = inner.upgrade() {
                    *c.update_needed.borrow_mut() = Some(reason.clone());
                }
            }));
        }
        Ok(Zero {
            engine,
            context,
            connection,
        })
    }

    pub fn client_id(&self) -> &str {
        &self.engine.client_id
    }

    pub fn client_group_id(&self) -> &str {
        &self.engine.client_group_id
    }

    pub fn user_id(&self) -> Option<&str> {
        self.engine.user_id.as_deref()
    }

    pub fn schema(&self) -> &Arc<Schema> {
        &self.engine.schema
    }

    pub fn context(&self) -> &C {
        &self.context
    }

    pub fn closed(&self) -> bool {
        self.engine.closed()
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// A view of the query's rows that stays current. Named queries are
    /// synced from the server; an unnamed one only sees local data.
    pub fn materialize(&self, query: &AnyQuery, ttl: &Ttl) -> Result<MaterializedView, Error> {
        let e = &self.engine;
        e.event_loop.call(|| e.context.materialize(query, ttl))
    }

    /// The query's rows: what the replica holds now (`Unknown`), or once
    /// the server has confirmed them on this connection (`Complete`) or
    /// any (`Cached`). Await the result, or read it once settled.
    ///
    /// An unnamed query only sees local data: the server never confirms
    /// it, so waiting for `Complete` or `Cached` never settles.
    pub fn run(
        &self,
        query: &AnyQuery,
        run_type: RunType,
        ttl: Option<&Ttl>,
    ) -> Deferred<ViewValue> {
        let e = &self.engine;
        e.event_loop.call(|| e.context.run(query, run_type, ttl))
    }

    /// Keeps the query's rows synced without a view, until `cleanup`.
    /// Preloading an unnamed query does nothing.
    pub fn preload(&self, query: &AnyQuery, ttl: &Ttl) -> Preload {
        let e = &self.engine;
        e.event_loop.call(|| e.context.preload(query, ttl))
    }

    /// Runs a mutator on the client now and sends it to the server. A
    /// mutator not registered with this client doesn't run: both outcomes
    /// are a `ZeroError` saying so. JS throws that error instead.
    pub fn mutate(&self, name: &str, args: Option<Value>) -> MutatorResult {
        let e = &self.engine;
        e.event_loop.call(|| e.mutate(name, args))
    }

    /// The app was shown, or hidden. Hidden for
    /// `ZeroOptions::hidden_disconnect_delay_ms`, the client disconnects
    /// until it is shown again.
    pub fn set_visible(&self, visible: bool) {
        let e = &self.engine;
        e.event_loop.call(|| e.set_visible(visible));
    }

    /// The network may be back: reconnect without waiting for the backoff.
    pub fn reconnect_now(&self) {
        let e = &self.engine;
        e.event_loop.call(|| e.reconnect_now());
    }

    /// Calls `done` once the server has confirmed every pending mutation,
    /// or when it can't.
    pub fn when_pending_settled(&self, done: impl FnOnce() + 'static) {
        let e = &self.engine;
        e.event_loop.call(|| e.when_pending_settled(done));
    }

    /// Runs `body` as a task of the client's event loop: the work it
    /// queues runs before this returns, as after a timer or a socket event.
    #[doc(hidden)]
    pub fn dispatch<T>(&self, body: impl FnOnce() -> T) -> T {
        self.engine.event_loop.dispatch(body)
    }

    /// Closes the connection. Pending mutations and queries fail, and
    /// views stop updating.
    pub fn close(&self) {
        let e = &self.engine;
        e.event_loop.call(|| e.close());
    }

    /// Waits for the local store's queued writes, such as before the app
    /// exits. A failure to write them is reported to
    /// `ZeroOptions::on_storage_failure`.
    pub fn flush_storage(&self) {
        self.engine.database.flush();
    }

    /// Closes the client and deletes its local data (`zero.delete()`): the
    /// replica and pending mutations of this user and storage key, under
    /// every schema version. Pending mutations the server hasn't confirmed
    /// are lost. Use it to log a user out.
    pub fn delete(&self) -> DroppedDatabases {
        self.close();
        let name = replicache_name(&self.engine.database.name);
        ReplicaDatabase::drop_matching(&self.engine.options.storage, &|n| n == name)
    }
}

impl<C> Drop for Zero<C> {
    /// Dropping the client closes it: otherwise its socket would stay
    /// open, and its timers run, with no client left to hear them.
    fn drop(&mut self) {
        let e = &self.engine;
        e.event_loop.call(|| e.close());
    }
}

type StateListener = Rc<dyn Fn(&ConnectionState)>;

struct ConnectionInner {
    engine: Rc<Engine>,
    state: RefCell<ConnectionState>,
    update_needed: RefCell<Option<UpdateNeededReason>>,
    listeners: RefCell<Vec<(u64, StateListener)>>,
    next_id: Cell<u64>,
}

/// The connection to zero-cache (`Connection` in
/// `packages/zero-client/src/client/connection.ts`).
#[derive(Clone)]
pub struct Connection(Rc<ConnectionInner>);

impl Connection {
    fn new(engine: Rc<Engine>) -> Self {
        let inner = Rc::new(ConnectionInner {
            state: RefCell::new(ConnectionState::of(&engine.connection_manager.state())),
            engine: engine.clone(),
            update_needed: RefCell::new(None),
            listeners: RefCell::default(),
            next_id: Cell::new(0),
        });
        let weak = Rc::downgrade(&inner);
        // The manager lives as long as the engine, which the connection
        // keeps.
        let _ = engine.connection_manager.subscribe(move |s| {
            let Some(inner) = weak.upgrade() else { return };
            let state = ConnectionState::of(s);
            *inner.state.borrow_mut() = state.clone();
            let listeners: Vec<_> = inner
                .listeners
                .borrow()
                .iter()
                .map(|(_, l)| l.clone())
                .collect();
            for l in listeners {
                l(&state);
            }
        });
        Connection(inner)
    }

    pub fn state(&self) -> ConnectionState {
        self.0.state.borrow().clone()
    }

    /// Why the app must be updated to sync, once the server said so
    /// (`onUpdateNeeded`). Reconnecting doesn't help.
    pub fn update_needed(&self) -> Option<UpdateNeededReason> {
        self.0.update_needed.borrow().clone()
    }

    /// Calls `listener` on every change of `state`. Returns the function
    /// that removes it.
    pub fn subscribe(&self, listener: impl Fn(&ConnectionState) + 'static) -> Box<dyn Fn()> {
        let id = self.0.next_id.get();
        self.0.next_id.set(id + 1);
        self.0.listeners.borrow_mut().push((id, Rc::new(listener)));
        let weak = Rc::downgrade(&self.0);
        Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.listeners.borrow_mut().retain(|(i, _)| *i != id);
            }
        })
    }

    /// Resumes connecting from `needs-auth` or `error`. With `auth`, sets
    /// the token first; when already connected, the server gets the new
    /// token.
    pub fn connect(&self, auth: Option<String>) {
        let e = &self.0.engine;
        e.event_loop.call(|| e.connect(auth.map(Some)));
    }
}
