//! The sync thread: the Zero client, off the UI thread.
//!
//! `zero-client` is single-threaded and its tokio transport runs on a
//! `LocalSet`, while Rune owns the main thread and runs no executor. So the
//! client lives on a thread of its own, with a current-thread runtime. The
//! UI sends it commands (closures that run with the client), and it sends
//! back each view's rows, read as the generated row types, with
//! `AppHandle::update`, which runs them on the UI thread before the next
//! frame. Reading a poke into IVM and the rows out of it stays off the UI
//! thread.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use rune::AppHandle;
use tokio::sync::mpsc;
use zero_client::client::{
    ConnectionState, MaterializedView, MutationOutcome, Mutator, MutatorResult, Preload, Zero,
    ZeroOptions, ZeroStorage,
};
use zero_client::ivm::ResultType;
use zero_client::query::AnyQuery;
use zero_client::schema::Schema;
use zero_client::tokio_transport::{TokioScheduler, TokioSocketFactory};
use zero_client::ttl::Ttl;
use zero_client::typed::{Query, QueryValue};

/// The app's state, which the sync thread updates.
pub type Ui<S> = AppHandle<S>;

/// What a client is made for: the servers, and who is logged in.
#[derive(Clone)]
pub struct Session<C> {
    pub cache_url: String,
    pub api_url: String,
    pub token: Option<String>,
    pub user_id: Option<String>,
    /// The context of every query and mutator.
    pub context: C,
}

type Command<S, C> = Box<dyn FnOnce(&mut Worker<S, C>) + Send>;

/// The UI's side of the sync thread. Cheap to clone; each call queues a
/// command, which the sync thread runs in order.
pub struct SyncThread<S: 'static, C: 'static> {
    commands: mpsc::UnboundedSender<Command<S, C>>,
}

impl<S, C> Clone for SyncThread<S, C> {
    fn clone(&self) -> Self {
        SyncThread {
            commands: self.commands.clone(),
        }
    }
}

/// How the app makes a client for a session.
pub struct Setup<C: 'static> {
    pub schema: fn() -> Arc<Schema>,
    pub mutators: fn() -> Vec<Mutator<C>>,
    /// The queries to keep synced for a client's life, as zbugs preloads.
    pub preload: fn(&C) -> Vec<AnyQuery>,
    /// Keeps local stores of one app apart from another's.
    pub storage_key: &'static str,
    /// Where the SQLite replica is kept.
    pub storage_dir: std::path::PathBuf,
}

impl<S: 'static, C: Clone + Send + 'static> SyncThread<S, C> {
    /// Starts the sync thread. `on_connection` hears every connection state
    /// of the current client.
    pub fn start(ui: Ui<S>, setup: Setup<C>, on_connection: fn(&mut S, ConnectionState)) -> Self {
        let (commands, mut rx) = mpsc::unbounded_channel::<Command<S, C>>();
        std::thread::Builder::new()
            .name("zero-sync".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("the sync thread's runtime");
                let local = tokio::task::LocalSet::new();
                local.block_on(&runtime, async move {
                    let mut worker = Worker {
                        ui,
                        setup,
                        on_connection,
                        client: None,
                        watches: HashMap::new(),
                    };
                    while let Some(command) = rx.recv().await {
                        command(&mut worker);
                    }
                    worker.close();
                });
            })
            .expect("the sync thread");
        SyncThread { commands }
    }

    fn send(&self, command: impl FnOnce(&mut Worker<S, C>) + Send + 'static) {
        // The thread outlives the UI: a send only fails while quitting.
        let _ = self.commands.send(Box::new(command));
    }

    /// Replaces the client with one for `session`. Watches start over on it.
    pub fn connect(&self, session: Session<C>) {
        self.send(move |w| w.connect(session));
    }

    /// Keeps `slot` showing `query`'s rows: `apply` gets them now, from the
    /// local store, and on every change. Watching a slot again replaces its
    /// query; the old rows stay in the app's state until the new ones come.
    pub fn watch<R: 'static, T: QueryValue + Send + 'static>(
        &self,
        slot: &'static str,
        query: impl Fn(&C) -> Query<R, T> + Send + 'static,
        apply: fn(&mut S, T, ResultType, &mut rune::Tx),
    ) {
        self.send(move |w| w.watch(slot, query, apply));
    }

    pub fn unwatch(&self, slot: &'static str) {
        self.send(move |w| {
            w.watches.remove(slot);
        });
    }

    /// Runs a mutator. `on_error` hears why it failed, on the client or on
    /// the server.
    pub fn mutate(
        &self,
        run: impl FnOnce(&Zero<C>) -> MutatorResult + Send + 'static,
        on_error: fn(&mut S, String),
    ) {
        self.send(move |w| {
            let Some(client) = &w.client else { return };
            let result = run(&client.zero);
            let ui = w.ui.clone();
            result.client.on_settle(move |r| {
                if let Some(message) = failure(r, true) {
                    ui.update(move |s, _| on_error(s, message));
                }
            });
            // The server's outcome, unless the client's already failed (the
            // same error, twice). A Zero error there only means the result
            // didn't arrive, such as offline: the client sends the mutation
            // again when it reconnects.
            let (ui, client) = (w.ui.clone(), result.client);
            result.server.on_settle(move |r| {
                let client_failed = client.result().is_some_and(|c| failure(&c, true).is_some());
                if let Some(message) = failure(r, false).filter(|_| !client_failed) {
                    ui.update(move |s, _| on_error(s, message));
                }
            });
        });
    }

    /// The network may be back: retry now.
    pub fn reconnect_now(&self) {
        self.send(|w| {
            if let Some(c) = &w.client {
                c.zero.reconnect_now();
            }
        });
    }

    /// Closes the client and waits, at most two seconds, for its local
    /// store's writes, so pending mutations outlive the app.
    pub fn close(&self) {
        let (done, wait) = std::sync::mpsc::channel();
        self.send(move |w| {
            w.close();
            let _ = done.send(());
        });
        let _ = wait.recv_timeout(std::time::Duration::from_secs(2));
    }
}

/// Why a mutation failed, if it did. `zero_errors`: whether a Zero error
/// counts.
fn failure(
    r: &Result<MutationOutcome, zero_client::client::Error>,
    zero_errors: bool,
) -> Option<String> {
    match r {
        Ok(MutationOutcome::Success { .. }) => None,
        Ok(MutationOutcome::AppError { message, .. }) => Some(message.clone()),
        Ok(MutationOutcome::ZeroError { message }) => zero_errors.then(|| message.clone()),
        Err(e) => Some(e.message()),
    }
}

struct Client<C: 'static> {
    zero: Zero<C>,
    _preloads: Vec<Preload>,
    unsubscribe: Box<dyn Fn()>,
}

type Remake<C> = Rc<dyn Fn(&Zero<C>) -> Option<(MaterializedView, Box<dyn Fn()>)>>;

/// A slot's view, and how to make it again on a new client.
struct Watch<C> {
    view: Option<(MaterializedView, Box<dyn Fn()>)>,
    remake: Remake<C>,
}

impl<C> Drop for Watch<C> {
    fn drop(&mut self) {
        if let Some((view, remove)) = self.view.take() {
            remove();
            view.destroy();
        }
    }
}

pub struct Worker<S: 'static, C: 'static> {
    ui: Ui<S>,
    setup: Setup<C>,
    on_connection: fn(&mut S, ConnectionState),
    client: Option<Client<C>>,
    watches: HashMap<&'static str, Watch<C>>,
}

impl<S: 'static, C: Clone + 'static> Worker<S, C> {
    fn connect(&mut self, session: Session<C>) {
        // The views are of the old client: drop them before it closes.
        let remakes: Vec<_> = self
            .watches
            .drain()
            .map(|(slot, w)| (slot, w.remake.clone()))
            .collect();
        self.close();

        let options = ZeroOptions {
            cache_url: Some(session.cache_url.clone()),
            user_id: session.user_id.clone(),
            auth: session.token.clone(),
            mutate_url: Some(format!("{}/api/mutate", session.api_url)),
            query_url: Some(format!("{}/api/query", session.api_url)),
            storage_key: Some(self.setup.storage_key.to_owned()),
            storage: ZeroStorage::Sqlite {
                directory: Some(self.setup.storage_dir.clone()),
            },
            ..ZeroOptions::default()
        };
        let zero = match Zero::new(
            (self.setup.schema)(),
            session.context.clone(),
            (self.setup.mutators)(),
            options,
            Rc::new(TokioScheduler::new()),
            Some(Rc::new(TokioSocketFactory::new())),
        ) {
            Ok(z) => z,
            Err(e) => {
                // A mutator registered twice, or a cache URL that isn't
                // http:// or https://, fails here.
                let reason = e.message();
                let f = self.on_connection;
                self.ui
                    .update(move |s, _| f(s, ConnectionState::Error { reason }));
                return;
            }
        };
        let ttl = Ttl::seconds(10.0);
        let preloads = (self.setup.preload)(&session.context)
            .iter()
            .map(|q| zero.preload(q, &ttl))
            .collect();
        let (ui, f) = (self.ui.clone(), self.on_connection);
        let state = zero.connection().state();
        ui.update(move |s, _| f(s, state));
        let ui = self.ui.clone();
        let unsubscribe = zero.connection().subscribe(move |state| {
            let state = state.clone();
            ui.update(move |s, _| f(s, state));
        });
        self.client = Some(Client {
            zero,
            _preloads: preloads,
            unsubscribe,
        });
        for (slot, remake) in remakes {
            self.install(slot, remake);
        }
    }

    fn watch<R: 'static, T: QueryValue + Send + 'static>(
        &mut self,
        slot: &'static str,
        query: impl Fn(&C) -> Query<R, T> + Send + 'static,
        apply: fn(&mut S, T, ResultType, &mut rune::Tx),
    ) {
        let ui = self.ui.clone();
        // Built for each client, with its context: logging in changes what
        // a query asks for.
        let remake: Remake<C> = Rc::new(move |zero| {
            let query = query(zero.context());
            let view = zero.materialize(query.any(), &Ttl::seconds(10.0)).ok()?;
            let ui = ui.clone();
            let remove = view.add_listener(move |data, result_type, _| {
                // Rows the generated types can't read are a bug of the
                // port; say so rather than show nothing.
                match query.read(data) {
                    Ok(rows) => ui.update(move |s, tx| apply(s, rows, result_type, tx)),
                    Err(e) => eprintln!("zbugs-rune: {slot}: {e}"),
                }
            });
            Some((view, remove))
        });
        self.watches.remove(slot);
        self.install(slot, remake);
    }

    fn install(&mut self, slot: &'static str, remake: Remake<C>) {
        let Some(client) = &self.client else {
            self.watches.insert(slot, Watch { view: None, remake });
            return;
        };
        let view = remake(&client.zero);
        self.watches.insert(slot, Watch { view, remake });
    }

    fn close(&mut self) {
        self.watches.clear();
        if let Some(c) = self.client.take() {
            (c.unsubscribe)();
            c.zero.flush_storage();
            c.zero.close();
        }
    }
}
