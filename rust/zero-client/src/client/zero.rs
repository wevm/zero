//! The public client (`Zero` in `packages/zero-client/src/client/zero.ts`)
//! and its connection (`connection.ts`).

use super::connection_manager::ConnectionState;
use super::database::{ReplicaDatabase, replicache_name};
use super::engine::{Engine, MutatorFn, MutatorResult, UpdateNeededReason, ZeroOptions};
use super::query_context::{MaterializedView, Preload, RunType, WeakView};
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
    /// The name the server runs it by: its path in the TS mutator registry.
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
    core: Rc<Core>,
    context: Rc<C>,
}

/// What the client keeps to replace its engine by a new one on the same
/// store when the server lost its state (`replaceClient` in zero-swift's
/// `ZeroClient`): the views and preloads move to the new engine.
struct Core {
    engine: RefCell<Rc<Engine>>,
    schema: Arc<Schema>,
    options: ZeroOptions,
    functions: Rc<HashMap<String, MutatorFn>>,
    scheduler: Rc<dyn Scheduler>,
    socket_factory: Option<Rc<dyn SocketFactory>>,
    connection: Connection,
    views: RefCell<Vec<(WeakView, AnyQuery, Ttl)>>,
    preloads: RefCell<Vec<Rc<PreloadRecord>>>,
}

/// A preload, which a replacement starts again on the new engine until
/// its cleanup.
struct PreloadRecord {
    query: AnyQuery,
    ttl: Ttl,
    cleanup: RefCell<Rc<dyn Fn()>>,
    active: Cell<bool>,
}

impl<C: 'static> Zero<C> {
    /// - `schema`: the app's schema.
    /// - `mutators`: every mutator the client may run. Pending mutations
    ///   are replayed by name, so each must be registered.
    /// - `scheduler`: the clock and timers, on the app's event loop; tests
    ///   pass a [`ManualScheduler`](crate::testing::ManualScheduler).
    /// - `socket_factory`: opens the sync socket. Without one the client
    ///   runs locally.
    ///
    /// When the server no longer has the client's state, such as after it
    /// closed the connection for a message over its size limit, and the
    /// app set no `on_client_state_not_found`, the client replaces itself
    /// by a new one on the same store, as JS reloads the page: a new client
    /// ID and client group, the views and preloads kept, and pending
    /// mutations of the old client group dropped.
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
        let functions = Rc::new(functions);
        let engine = Engine::new(
            schema.clone(),
            options.clone(),
            functions.clone(),
            scheduler.clone(),
            socket_factory.clone(),
            true,
        )?;
        let connection = Connection::new(engine.clone());
        let core = Rc::new(Core {
            engine: RefCell::new(engine.clone()),
            schema,
            options,
            functions,
            scheduler,
            socket_factory,
            connection,
            views: RefCell::default(),
            preloads: RefCell::default(),
        });
        Core::attach(&core, &engine);
        Ok(Zero { core, context })
    }

    fn engine(&self) -> Rc<Engine> {
        self.core.engine()
    }

    /// The client's ID, which changes when it replaces itself.
    pub fn client_id(&self) -> String {
        self.engine().client_id.clone()
    }

    /// The client group's ID, which changes when the client replaces
    /// itself.
    pub fn client_group_id(&self) -> String {
        self.engine().client_group_id.clone()
    }

    /// The user the client syncs for.
    pub fn user_id(&self) -> Option<&str> {
        self.core.options.user_id.as_deref()
    }

    /// The app's schema.
    pub fn schema(&self) -> &Arc<Schema> {
        &self.core.schema
    }

    /// What the mutators are given besides their arguments.
    pub fn context(&self) -> &C {
        &self.context
    }

    /// Whether the client is closed.
    pub fn closed(&self) -> bool {
        self.engine().closed()
    }

    /// The client's connection to zero-cache.
    pub fn connection(&self) -> &Connection {
        &self.core.connection
    }

    /// A view of the query's rows that stays current. Named queries are
    /// synced from the server; an unnamed one only sees local data.
    pub fn materialize(&self, query: &AnyQuery, ttl: &Ttl) -> Result<MaterializedView, Error> {
        let e = self.engine();
        let view = e.event_loop.call(|| e.context.materialize(query, ttl))?;
        let mut views = self.core.views.borrow_mut();
        views.retain(|(v, _, _)| v.upgrade().is_some_and(|v| !v.is_destroyed()));
        views.push((view.downgrade(), query.clone(), ttl.clone()));
        Ok(view)
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
        let e = self.engine();
        e.event_loop.call(|| e.context.run(query, run_type, ttl))
    }

    /// Keeps the query's rows synced without a view, until `cleanup`.
    /// Preloading an unnamed query does nothing.
    pub fn preload(&self, query: &AnyQuery, ttl: &Ttl) -> Preload {
        let e = self.engine();
        let preload = e.event_loop.call(|| e.context.preload(query, ttl));
        let record = Rc::new(PreloadRecord {
            query: query.clone(),
            ttl: ttl.clone(),
            cleanup: RefCell::new(preload.cleanup),
            active: Cell::new(true),
        });
        let mut preloads = self.core.preloads.borrow_mut();
        preloads.retain(|p| p.active.get());
        preloads.push(record.clone());
        Preload {
            cleanup: Rc::new(move || {
                record.active.set(false);
                let cleanup = record.cleanup.borrow().clone();
                cleanup();
            }),
            cached: preload.cached,
            complete: preload.complete,
        }
    }

    /// Runs a mutator on the client now and sends it to the server. A
    /// mutator not registered with this client doesn't run: both outcomes
    /// are a `ZeroError` saying so. JS throws that error instead.
    pub fn mutate(&self, name: &str, args: Option<Value>) -> MutatorResult {
        let e = self.engine();
        e.event_loop.call(|| e.mutate(name, args))
    }

    /// The app was shown, or hidden. Hidden for
    /// `ZeroOptions::hidden_disconnect_delay_ms`, the client disconnects
    /// until it is shown again.
    pub fn set_visible(&self, visible: bool) {
        let e = self.engine();
        e.event_loop.call(|| e.set_visible(visible));
    }

    /// The network may be back: reconnect without waiting for the backoff.
    pub fn reconnect_now(&self) {
        let e = self.engine();
        e.event_loop.call(|| e.reconnect_now());
    }

    /// Calls `done` once the server has confirmed every pending mutation,
    /// or when it can't.
    pub fn when_pending_settled(&self, done: impl FnOnce() + 'static) {
        let e = self.engine();
        e.event_loop.call(|| e.when_pending_settled(done));
    }

    /// Runs `body` as a task of the client's event loop: the work it
    /// queues runs before this returns, as after a timer or a socket event.
    #[doc(hidden)]
    pub fn dispatch<T>(&self, body: impl FnOnce() -> T) -> T {
        self.engine().event_loop.dispatch(body)
    }

    /// Closes the connection. Pending mutations and queries fail, and
    /// views stop updating.
    pub fn close(&self) {
        let e = self.engine();
        e.event_loop.call(|| e.close());
    }

    /// Waits for the local store's queued writes, such as before the app
    /// exits. A failure to write them is reported to
    /// `ZeroOptions::on_storage_failure`.
    pub fn flush_storage(&self) {
        self.engine().database.flush();
    }

    /// Closes the client and deletes its local data (`zero.delete()`): the
    /// replica and pending mutations of this user and storage key, under
    /// every schema version. Pending mutations the server hasn't confirmed
    /// are lost. Use it to log a user out.
    pub fn delete(&self) -> DroppedDatabases {
        self.close();
        let name = replicache_name(&self.engine().database.name);
        ReplicaDatabase::drop_matching(&self.core.options.storage, &|n| n == name)
    }
}

impl Core {
    fn engine(&self) -> Rc<Engine> {
        self.engine.borrow().clone()
    }

    /// Hooks `engine` up to the client: the connection hears it needs an
    /// update, and the client replaces it when it lost its state.
    fn attach(core: &Rc<Core>, engine: &Rc<Engine>) {
        let connection = Rc::downgrade(&core.connection.0);
        *engine.on_update_needed.borrow_mut() = Some(Rc::new(move |reason| {
            if let Some(c) = connection.upgrade() {
                *c.update_needed.borrow_mut() = Some(reason.clone());
            }
        }));
        if core.options.on_client_state_not_found.is_none() {
            let core = Rc::downgrade(core);
            *engine.on_client_state_not_found.borrow_mut() = Some(Rc::new(move || {
                if let Some(core) = core.upgrade() {
                    Core::replace(&core);
                }
            }));
        }
    }

    /// Replaces the engine, which can't sync any longer, by a new one on
    /// the same store, and moves the views and preloads to it.
    fn replace(core: &Rc<Core>) {
        let old = core.engine();
        if old.closed() {
            return;
        }
        // The new engine goes on with the token the app last gave, not the
        // one it started with.
        let mut options = core.options.clone();
        let auth = old.auth.borrow().clone();
        options.auth = (!auth.is_empty()).then_some(auth);
        let new = match Engine::new(
            core.schema.clone(),
            options,
            core.functions.clone(),
            core.scheduler.clone(),
            core.socket_factory.clone(),
            old.visible.get(),
        ) {
            Ok(new) => new,
            Err(e) => {
                old.log
                    .error(|| format!("Failed to replace the client: {e}"));
                return;
            }
        };
        *core.engine.borrow_mut() = new.clone();
        Core::attach(core, &new);
        core.connection.bind(new.clone());
        let views: Vec<_> = {
            let mut views = core.views.borrow_mut();
            views.retain(|(v, _, _)| v.upgrade().is_some_and(|v| !v.is_destroyed()));
            views
                .iter()
                .filter_map(|(v, q, t)| Some((v.upgrade()?, q.clone(), t.clone())))
                .collect()
        };
        for (view, query, ttl) in views {
            if let Ok(fresh) = new
                .event_loop
                .call(|| new.context.materialize(&query, &ttl))
            {
                view.rebind(fresh);
            }
        }
        let preloads: Vec<_> = {
            let mut preloads = core.preloads.borrow_mut();
            preloads.retain(|p| p.active.get());
            preloads.clone()
        };
        for record in preloads {
            let preload = new
                .event_loop
                .call(|| new.context.preload(&record.query, &record.ttl));
            *record.cleanup.borrow_mut() = preload.cleanup;
        }
        old.event_loop.call(|| old.close());
    }
}

impl<C> Drop for Zero<C> {
    /// Dropping the client closes it: otherwise its socket would stay
    /// open, and its timers run, with no client left to hear them.
    fn drop(&mut self) {
        let e = self.core.engine();
        e.event_loop.call(|| e.close());
    }
}

type StateListener = Rc<dyn Fn(&ConnectionState)>;

struct ConnectionInner {
    engine: RefCell<Rc<Engine>>,
    /// Counts the engines bound, so a replaced one isn't heard.
    generation: Cell<u64>,
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
            engine: RefCell::new(engine.clone()),
            generation: Cell::new(0),
            update_needed: RefCell::new(None),
            listeners: RefCell::default(),
            next_id: Cell::new(0),
        });
        let connection = Connection(inner);
        connection.listen(&engine);
        connection
    }

    /// Follows `engine`'s state from now on, and reports it.
    fn bind(&self, engine: Rc<Engine>) {
        *self.0.engine.borrow_mut() = engine.clone();
        self.0.generation.set(self.0.generation.get() + 1);
        self.listen(&engine);
        self.0
            .changed(ConnectionState::of(&engine.connection_manager.state()));
    }

    fn listen(&self, engine: &Engine) {
        let weak = Rc::downgrade(&self.0);
        let generation = self.0.generation.get();
        // The manager lives as long as the engine, which the connection
        // keeps until it binds another.
        let _ = engine.connection_manager.subscribe(move |s| {
            let Some(inner) = weak.upgrade() else { return };
            if inner.generation.get() == generation {
                inner.changed(ConnectionState::of(s));
            }
        });
    }

    /// The state now.
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
        let e = self.0.engine.borrow().clone();
        e.event_loop.call(|| e.connect(auth.map(Some)));
    }
}

impl ConnectionInner {
    fn changed(&self, state: ConnectionState) {
        *self.state.borrow_mut() = state.clone();
        let listeners: Vec<_> = self
            .listeners
            .borrow()
            .iter()
            .map(|(_, l)| l.clone())
            .collect();
        for l in listeners {
            l(&state);
        }
    }
}
