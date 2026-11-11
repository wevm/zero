//! Materializes queries over an IVM branch, registers named ones with the
//! query manager, and flushes views when a transaction commits
//! (`ZeroContext` in `packages/zero-client/src/client/context.ts`, with
//! `materializeImpl`, `runImpl`, and `preloadImpl` from
//! `packages/zql/src/query/query-delegate-base.ts`).

use super::ivm_branch::IvmBranch;
use super::log::Log;
use super::query_manager::{Got, GotCallback, closed_error};
use super::replica_store::KvDiff;
use super::{ConnectionError, Deferred, Error, QueryError};
use crate::builder::{BuilderDelegate, OptimisticAggregate, build_pipeline, group_view_input};
use crate::hash::query::{hash_of_ast, hash_of_name_and_args};
use crate::ivm::{
    ArrayView, DeferredInput, Input, PipelineError, ResultType, Row, Source, ViewValue, capture,
};
use crate::map::OrderedMap;
use crate::protocol::ErroredQuery;
use crate::query::{AnyQuery, CustomQueryId};
use crate::schema::Column;
use crate::ttl::Ttl;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

/// Builds pipelines over the sources of an IVM branch.
struct BranchDelegate {
    branch: Rc<IvmBranch>,
    /// The synced client reads aggregates from the rows the server streams;
    /// a mutator's query the server hasn't answered computes them.
    aggregates_from_source: bool,
}

impl BuilderDelegate for BranchDelegate {
    fn get_source(&self, table_name: &str) -> Option<Rc<dyn Source>> {
        self.branch.source(table_name).map(|s| s as Rc<dyn Source>)
    }

    fn aggregates_from_source(&self) -> bool {
        self.aggregates_from_source
    }

    fn get_aggregate_source(
        &self,
        name: &str,
        columns: OrderedMap<Column>,
        primary_key: Vec<String>,
        optimistic: Option<OptimisticAggregate>,
    ) -> Option<Rc<dyn Source>> {
        Some(
            self.branch
                .aggregate_source(name, columns, primary_key, optimistic),
        )
    }

    fn aggregate_key_columns(&self, table: &str, columns: &[String]) -> Vec<String> {
        self.branch.server_column_names(table, columns)
    }
}

/// Registers a named query with the server; returns its unregister.
pub(crate) type AddCustomQuery = Box<dyn Fn(&CustomQueryId, &Ttl, GotCallback) -> Box<dyn Fn()>>;
pub(crate) type UpdateCustomQuery = Rc<dyn Fn(&CustomQueryId, &Ttl)>;
pub(crate) type Microtask = Rc<dyn Fn(Box<dyn FnOnce()>)>;
/// Builds a deferred pipeline; returns the release to call once every
/// pending pipeline is built.
type AttachPipeline = Box<dyn FnOnce() -> Result<Box<dyn FnOnce()>, PipelineError>>;

/// How many view notifications are under way on a client.
pub(crate) type NotificationGate = Rc<Cell<u32>>;

type Callback = Rc<dyn Fn()>;
type RemovePendingAttach = Box<dyn Fn()>;

/// A query's pipeline and, when it is built later, its placeholder input.
type BuiltPipeline = (Rc<dyn Input>, Option<Rc<DeferredInput>>);

pub(crate) struct QueryContext {
    me: Weak<QueryContext>,
    pub(crate) branch: Rc<IvmBranch>,
    delegate: Rc<BranchDelegate>,
    add_custom_query: AddCustomQuery,
    update_custom_query: UpdateCustomQuery,
    microtask: Microtask,
    commit_listeners: RefCell<Vec<(u64, Callback)>>,
    next_listener_id: Cell<u64>,
    /// Until the replica is loaded into the sources, pipelines are built
    /// later, together (`mark_pipelines_ready`).
    pipelines_ready: Cell<bool>,
    pending_pipelines: RefCell<Vec<(u64, AttachPipeline)>>,
    log: Option<Log>,
    /// Why the client closed, once it has.
    closed_reason: Box<dyn Fn() -> Option<ConnectionError>>,
    /// The hashes of the unnamed queries already warned about.
    warned_local_only: RefCell<HashSet<String>>,
    pub(crate) notifications: NotificationGate,
    /// Why pushing into the branch failed, once it has (`#advanceError` in
    /// `ivm-branch.ts`): the branch is unusable from then on.
    branch_failure: RefCell<Option<PipelineError>>,
}

/// The settled state of a view's query: still waiting, confirmed by the
/// server, or failed.
#[derive(Clone)]
enum Completion {
    Pending,
    Complete,
    Failed(ErroredQuery),
}

/// What a view's query has heard, shared by the closures of `materialize`.
struct Track {
    attached: bool,
    got: Got,
    query_complete: Completion,
    resolved: Option<Completion>,
    on_resolved: Option<Rc<dyn Fn(Completion)>>,
    view: Weak<ArrayView>,
    destroyed: bool,
}

type TrackRef = Rc<RefCell<Track>>;

fn settle(track: &TrackRef, r: Completion) {
    let on_resolved = {
        let mut t = track.borrow_mut();
        if t.resolved.is_some() {
            return;
        }
        t.resolved = Some(r.clone());
        t.on_resolved.clone()
    };
    if let Some(f) = on_resolved {
        f(r);
    }
}

fn maybe_resolve_complete(track: &TrackRef) {
    let resolve = {
        let mut t = track.borrow_mut();
        let ok = t.attached && t.got == Got::Yes && matches!(t.query_complete, Completion::Pending);
        if ok {
            t.query_complete = Completion::Complete;
        }
        ok
    };
    if resolve {
        settle(track, Completion::Complete);
    }
}

fn maybe_mark_cached(track: &TrackRef) {
    let view = {
        let t = track.borrow();
        if t.attached && t.got == Got::Cached && matches!(t.query_complete, Completion::Pending) {
            t.view.upgrade()
        } else {
            None
        }
    };
    if let Some(v) = view {
        v.mark_cached();
    }
}

fn query_id(query: &AnyQuery) -> String {
    match query.custom_query_id() {
        Some(id) => hash_of_name_and_args(&id.name, &id.args),
        None => hash_of_ast(query.ast()),
    }
}

/// Which rows `Zero::run` returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunType {
    /// What the local replica holds now.
    Unknown,
    /// Once the store holds a server-confirmed result, from this connection or
    /// an earlier one.
    Cached,
    /// Once the server has confirmed the result on this connection.
    Complete,
}

impl RunType {
    /// The type as JS names it.
    pub fn as_str(self) -> &'static str {
        match self {
            RunType::Unknown => "unknown",
            RunType::Cached => "cached",
            RunType::Complete => "complete",
        }
    }

    /// The type JS names `s`, if any.
    pub fn parse(s: &str) -> Option<RunType> {
        Some(match s {
            "unknown" => RunType::Unknown,
            "cached" => RunType::Cached,
            "complete" => RunType::Complete,
            _ => return None,
        })
    }
}

/// What `preload` returns.
#[derive(Clone)]
pub struct Preload {
    /// Stops syncing the query (after its TTL).
    pub cleanup: Rc<dyn Fn()>,
    /// Settles once the store holds a server-confirmed result, from this
    /// connection or an earlier one.
    pub cached: Deferred<()>,
    /// Settles once the server has confirmed the result on this connection.
    pub complete: Deferred<()>,
}

impl QueryContext {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        branch: Rc<IvmBranch>,
        add_custom_query: AddCustomQuery,
        update_custom_query: UpdateCustomQuery,
        microtask: Microtask,
        log: Option<Log>,
        closed_reason: Box<dyn Fn() -> Option<ConnectionError>>,
        aggregates_from_source: bool,
    ) -> Rc<Self> {
        Rc::new_cyclic(|me| QueryContext {
            me: me.clone(),
            delegate: Rc::new(BranchDelegate {
                branch: branch.clone(),
                aggregates_from_source,
            }),
            branch,
            add_custom_query,
            update_custom_query,
            microtask,
            commit_listeners: RefCell::default(),
            next_listener_id: Cell::new(0),
            pipelines_ready: Cell::new(false),
            pending_pipelines: RefCell::default(),
            log,
            closed_reason,
            warned_local_only: RefCell::default(),
            notifications: Rc::default(),
            branch_failure: RefCell::new(None),
        })
    }

    /// A context for the queries a mutator runs: nothing is registered with
    /// the server, and the branch is already populated.
    pub(crate) fn local(branch: Rc<IvmBranch>, aggregates_from_source: bool) -> Rc<Self> {
        let context = QueryContext::new(
            branch,
            Box::new(|_, _, _| Box::new(|| {})),
            Rc::new(|_, _| {}),
            Rc::new(|f| f()),
            None,
            Box::new(|| None),
            aggregates_from_source,
        );
        context.mark_pipelines_ready();
        context
    }

    /// Whether listeners are being told about a change. JS mutators are
    /// async, so a mutation made from a listener commits once the
    /// notification is over.
    pub(crate) fn is_notifying(&self) -> bool {
        self.notifications.get() > 0
    }

    pub(crate) fn branch_failure(&self) -> Option<PipelineError> {
        self.branch_failure.borrow().clone()
    }

    /// The error of a query waiting for the server on a closed client.
    fn closed_error(&self, query: &AnyQuery) -> Option<QueryError> {
        let reason = (self.closed_reason)()?;
        let id = query_id(query);
        let name = query.custom_query_id().map(|c| c.name.as_str());
        Some(QueryError::new(Some(closed_error(&id, name, &reason))))
    }

    /// Warns, once per query, that an unnamed query is waited for or
    /// preloaded: it only sees local data, so the server never confirms it.
    fn warn_local_only(&self, query: &AnyQuery, consequence: &str) {
        if query.custom_query_id().is_some() {
            return;
        }
        let Some(log) = &self.log else { return };
        let hash = hash_of_ast(query.ast());
        if !self.warned_local_only.borrow_mut().insert(hash.clone()) {
            return;
        }
        log.warn(|| {
            format!(
                "The unnamed query of \"{}\" (hash {hash}) only sees local data and is never \
                 complete: {consequence}. To sync it from the server, use a named query.",
                query.table()
            )
        });
    }

    pub(crate) fn on_transaction_commit(&self, listener: impl Fn() + 'static) -> Box<dyn Fn()> {
        let id = self.next_listener_id.get();
        self.next_listener_id.set(id + 1);
        self.commit_listeners
            .borrow_mut()
            .push((id, Rc::new(listener)));
        let me = self.me.clone();
        Box::new(move || {
            if let Some(me) = me.upgrade() {
                me.commit_listeners.borrow_mut().retain(|(i, _)| *i != id);
            }
        })
    }

    fn on_pipelines_ready(&self, attach: AttachPipeline) -> Box<dyn Fn()> {
        assert!(
            !self.pipelines_ready.get(),
            "onPipelinesReady called while pipelines are ready"
        );
        let id = self.next_listener_id.get();
        self.next_listener_id.set(id + 1);
        self.pending_pipelines.borrow_mut().push((id, attach));
        let me = self.me.clone();
        Box::new(move || {
            if let Some(me) = me.upgrade() {
                me.pending_pipelines.borrow_mut().retain(|(i, _)| *i != id);
            }
        })
    }

    /// Builds and hydrates every deferred pipeline, in materialization
    /// order, as one batch of view updates.
    pub(crate) fn mark_pipelines_ready(&self) {
        if self.pipelines_ready.get() {
            return;
        }
        if self.pending_pipelines.borrow().is_empty() {
            self.pipelines_ready.set(true);
            return;
        }
        let mut releases = Vec::new();
        loop {
            let next = {
                let mut p = self.pending_pipelines.borrow_mut();
                if p.is_empty() {
                    break;
                }
                p.remove(0)
            };
            // A failing view has already been marked as errored; the others
            // are still built.
            if let Ok(release) = (next.1)() {
                releases.push(release);
            }
        }
        self.pipelines_ready.set(true);
        for r in releases {
            r();
        }
        self.end_transaction();
    }

    /// Pushes `diff` into the branch and flushes the views. A push an
    /// operator fails still flushes what the views got, and then fails.
    pub(crate) fn process_changes(
        &self,
        diff: &KvDiff,
        rows: &HashMap<String, Row>,
    ) -> Result<(), PipelineError> {
        if let Some(f) = self.branch_failure() {
            return Err(f);
        }
        let result = capture(|| self.branch.apply(diff, rows));
        self.end_transaction();
        if let Err(f) = result {
            *self.branch_failure.borrow_mut() = Some(f.clone());
            return Err(f);
        }
        Ok(())
    }

    fn end_transaction(&self) {
        self.notifications.set(self.notifications.get() + 1);
        let listeners: Vec<Rc<dyn Fn()>> = self
            .commit_listeners
            .borrow()
            .iter()
            .map(|(_, l)| l.clone())
            .collect();
        for l in listeners {
            l();
        }
        self.notifications.set(self.notifications.get() - 1);
    }

    pub(crate) fn materialize(
        &self,
        query: &AnyQuery,
        ttl: &Ttl,
    ) -> Result<MaterializedView, Error> {
        if let Some(message) = query.invalid() {
            return Err(PipelineError::new(message).into());
        }
        if let Some(f) = self.branch_failure() {
            return Err(f.into());
        }
        let custom_query_id = query.custom_query_id().cloned();
        let ast = query.ast().clone();
        let query_id = query_id(query);
        let microtask = self.microtask.clone();
        let defer_pipeline = !self.pipelines_ready.get();
        let track: TrackRef = Rc::new(RefCell::new(Track {
            attached: !defer_pipeline,
            got: Got::No,
            query_complete: Completion::Pending,
            resolved: None,
            on_resolved: None,
            view: Weak::new(),
            destroyed: false,
        }));
        let got_callback: GotCallback = {
            let track = track.clone();
            Rc::new(move |value: Got, error: Option<ErroredQuery>| {
                // Removals are deferred while mutations are pending, so a
                // destroyed view can still hear from the query manager.
                if track.borrow().destroyed {
                    return;
                }
                if let Some(error) = error {
                    settle(&track, Completion::Failed(error.clone()));
                    track.borrow_mut().query_complete = Completion::Failed(error);
                    return;
                }
                track.borrow_mut().got = value;
                match value {
                    Got::Cached => maybe_mark_cached(&track),
                    Got::No => {
                        let v = track.borrow().view.upgrade();
                        if let Some(v) = v {
                            v.unmark_cached();
                        }
                    }
                    Got::Yes => maybe_resolve_complete(&track),
                }
            })
        };
        let remove_added_query: Rc<dyn Fn()> = match &custom_query_id {
            Some(id) => Rc::from((self.add_custom_query)(id, ttl, got_callback)),
            None => Rc::new(|| {}),
        };
        let built = (|| -> Result<BuiltPipeline, PipelineError> {
            let (pipeline, deferred): (Rc<dyn Input>, _) = if defer_pipeline {
                let delegate = self.delegate.clone();
                let ast = ast.clone();
                let query_id = query_id.clone();
                let build = move || build_pipeline(&ast, delegate.as_ref(), &query_id);
                // A probe gets the schema, and makes a query that cannot be
                // built fail now rather than later.
                let probe = build()?;
                let schema = probe.get_schema();
                probe.destroy();
                let deferred = DeferredInput::new(schema, Box::new(build));
                (deferred.clone(), Some(deferred))
            } else {
                (
                    build_pipeline(&ast, self.delegate.as_ref(), &query_id)?,
                    None,
                )
            };
            // A view of a grouped query shows only its columns and value.
            let input = if ast.group_by.is_some() {
                group_view_input(pipeline, &ast, self.delegate.as_ref())?
            } else {
                pipeline
            };
            Ok((input, deferred))
        })();
        let (input, deferred) = match built {
            Ok(b) => b,
            Err(e) => {
                remove_added_query();
                return Err(e.into());
            }
        };
        let complete = track.borrow().query_complete.clone();
        let format = query.format().clone();
        let hydrated = capture(|| match &complete {
            Completion::Complete => ArrayView::new(input.clone(), format.clone(), None, true),
            Completion::Failed(e) => {
                ArrayView::new(input.clone(), format.clone(), Some(e.clone()), false)
            }
            Completion::Pending => ArrayView::new(input.clone(), format.clone(), None, false),
        });
        let created = match hydrated {
            Ok(v) => v,
            Err(e) => {
                // Unlike JS, which leaves the query registered, nothing is
                // left behind: there is no view to destroy.
                track.borrow_mut().destroyed = true;
                input.destroy();
                remove_added_query();
                return Err(e.into());
            }
        };
        if matches!(complete, Completion::Pending) {
            let weak = Rc::downgrade(&created);
            let microtask = microtask.clone();
            let on_resolved: Rc<dyn Fn(Completion)> = Rc::new(move |r| match r {
                Completion::Complete => {
                    let weak = weak.clone();
                    microtask(Box::new(move || {
                        if let Some(v) = weak.upgrade() {
                            v.set_result_type(ResultType::Complete, None);
                        }
                    }));
                }
                Completion::Failed(e) => {
                    let weak = weak.clone();
                    let inner = microtask.clone();
                    microtask(Box::new(move || {
                        inner(Box::new(move || {
                            if let Some(v) = weak.upgrade() {
                                v.set_result_type(ResultType::Error, Some(e));
                            }
                        }));
                    }));
                }
                Completion::Pending => {}
            });
            let resolved = {
                let mut t = track.borrow_mut();
                t.on_resolved = Some(on_resolved.clone());
                t.resolved.clone()
            };
            if let Some(r) = resolved {
                on_resolved(r);
            }
        }
        let remove_commit_observer = {
            let weak = Rc::downgrade(&created);
            self.on_transaction_commit(move || {
                if let Some(v) = weak.upgrade() {
                    v.flush();
                }
            })
        };
        let remove_pending_attach: Rc<RefCell<Option<RemovePendingAttach>>> = Rc::default();
        {
            let track = track.clone();
            let remove_pending_attach = remove_pending_attach.clone();
            let input = input.clone();
            let remove_added_query = remove_added_query.clone();
            created.set_on_destroy(move || {
                track.borrow_mut().destroyed = true;
                if let Some(r) = remove_pending_attach.borrow_mut().take() {
                    r();
                }
                input.destroy();
                remove_commit_observer();
                remove_added_query();
            });
        }
        if let Some(deferred) = deferred {
            let weak = Rc::downgrade(&created);
            let track = track.clone();
            let remove = remove_pending_attach.clone();
            let name = custom_query_id
                .as_ref()
                .map(|c| c.name.clone())
                .unwrap_or_default();
            let query_id = query_id.clone();
            let attach: AttachPipeline = Box::new(move || {
                remove.borrow_mut().take();
                if deferred.destroyed() {
                    return Ok(Box::new(|| {}));
                }
                if let Some(v) = weak.upgrade() {
                    v.hold_data();
                }
                if let Err(error) = deferred.attach() {
                    let e = ErroredQuery::App {
                        id: query_id,
                        name,
                        message: Some(error.to_string()),
                        details: None,
                    };
                    track.borrow_mut().query_complete = Completion::Failed(e.clone());
                    settle(&track, Completion::Failed(e));
                    return Err(error);
                }
                Ok(Box::new(move || {
                    if track.borrow().destroyed {
                        return;
                    }
                    track.borrow_mut().attached = true;
                    if let Some(v) = weak.upgrade() {
                        v.release_data();
                    }
                    maybe_resolve_complete(&track);
                    maybe_mark_cached(&track);
                }))
            });
            *remove_pending_attach.borrow_mut() = Some(self.on_pipelines_ready(attach));
        }
        track.borrow_mut().view = Rc::downgrade(&created);
        maybe_mark_cached(&track);
        let update_ttl: Box<dyn Fn(&Ttl)> = match custom_query_id {
            Some(id) => {
                let update = self.update_custom_query.clone();
                Box::new(move |ttl| update(&id, ttl))
            }
            None => Box::new(|_| {}),
        };
        Ok(MaterializedView::new(
            created,
            update_ttl,
            self.notifications.clone(),
        ))
    }

    /// `runImpl`: the rows now, or once the server has confirmed them.
    pub(crate) fn run(
        &self,
        query: &AnyQuery,
        run_type: RunType,
        ttl: Option<&Ttl>,
    ) -> Deferred<ViewValue> {
        let result = Deferred::new();
        // On a closed client, waiting for the server fails at once rather
        // than never settling, without materializing anything.
        if run_type != RunType::Unknown
            && let Some(error) = self.closed_error(query)
        {
            result.reject(error);
            return result;
        }
        let default = Ttl::String(Ttl::DEFAULT.into());
        let v = match self.materialize(query, ttl.unwrap_or(&default)) {
            Ok(v) => v,
            Err(e) => {
                // JS's `run` is async: a query that can't be materialized
                // rejects it.
                result.reject(e);
                return result;
            }
        };
        if run_type == RunType::Unknown {
            let data = v.data();
            v.destroy();
            result.resolve(data);
            return result;
        }
        self.warn_local_only(
            query,
            &format!("a run waiting for .{} never returns", run_type.as_str()),
        );
        {
            let v = v.clone();
            result.on_settle(move |_| v.destroy());
        }
        let r = result.clone();
        // Destroying the view on settle removes the listener.
        let _ = v.add_listener(move |data, result_type, error| {
            if result_type == ResultType::Complete
                || (run_type == RunType::Cached && result_type == ResultType::Cached)
            {
                r.resolve(data.clone());
            } else if result_type == ResultType::Error {
                r.reject(QueryError {
                    error: error.cloned(),
                    data: Some(data.clone()),
                });
            }
        });
        result
    }

    /// The rows of `query` now, for a query run inside a mutator.
    pub(crate) fn snapshot(&self, query: &AnyQuery) -> Result<ViewValue, Error> {
        let v = self.materialize(query, &Ttl::String(Ttl::DEFAULT.into()))?;
        let data = v.data();
        v.destroy();
        Ok(data)
    }

    /// `preloadImpl`: keeps a query's rows synced without a view.
    pub(crate) fn preload(&self, query: &AnyQuery, ttl: &Ttl) -> Preload {
        let cached = Deferred::new();
        let complete = Deferred::new();
        let noop: Rc<dyn Fn()> = Rc::new(|| {});
        // On a closed client, both fail at once and nothing is registered.
        if let Some(error) = self.closed_error(query) {
            cached.reject(error.clone());
            complete.reject(error);
            return Preload {
                cleanup: noop,
                cached,
                complete,
            };
        }
        if let Some(message) = query.invalid() {
            if let Some(log) = &self.log {
                log.error(|| message.to_owned());
            }
            cached.reject(PipelineError::new(message));
            complete.reject(PipelineError::new(message));
            return Preload {
                cleanup: noop,
                cached,
                complete,
            };
        }
        let Some(id) = query.custom_query_id() else {
            self.warn_local_only(
                query,
                "preloading it does nothing, and its cached and complete never settle",
            );
            return Preload {
                cleanup: noop,
                cached,
                complete,
            };
        };
        let (c, p) = (cached.clone(), complete.clone());
        let cleanup = (self.add_custom_query)(
            id,
            ttl,
            Rc::new(move |got, error| {
                if let Some(error) = error {
                    c.reject(QueryError::new(Some(error.clone())));
                    p.reject(QueryError::new(Some(error)));
                    return;
                }
                if got == Got::Yes {
                    c.resolve(());
                    p.resolve(());
                } else if got == Got::Cached {
                    c.resolve(());
                }
            }),
        );
        Preload {
            cleanup: Rc::from(cleanup),
            cached,
            complete,
        }
    }
}

/// Called with a view's data, result type, and error.
pub type ViewListener = Rc<dyn Fn(&ViewValue, ResultType, Option<&ErroredQuery>)>;

/// What a view reads from its client: replaced when the client is
/// (`rebind`).
struct ViewBinding {
    view: Rc<ArrayView>,
    update_ttl: Box<dyn Fn(&Ttl)>,
    notifications: NotificationGate,
}

struct ViewInner {
    binding: RefCell<ViewBinding>,
    remove_forward: RefCell<Option<Box<dyn Fn()>>>,
    destroyed: Cell<bool>,
    listeners: RefCell<Vec<(u64, ViewListener)>>,
    next_listener_id: Cell<u64>,
    /// How many notification passes are under way. As in JS, a listener
    /// that destroys the view doesn't keep the pass from reaching the
    /// others.
    forwarding: Cell<u32>,
}

/// A view of a query that stays current as data changes (`TypedView`).
#[derive(Clone)]
pub struct MaterializedView(Rc<ViewInner>);

/// A view that doesn't keep it alive.
pub(crate) struct WeakView(Weak<ViewInner>);

impl WeakView {
    pub(crate) fn upgrade(&self) -> Option<MaterializedView> {
        self.0.upgrade().map(MaterializedView)
    }
}

impl MaterializedView {
    fn new(
        view: Rc<ArrayView>,
        update_ttl: Box<dyn Fn(&Ttl)>,
        notifications: NotificationGate,
    ) -> Self {
        let inner = Rc::new(ViewInner {
            binding: RefCell::new(ViewBinding {
                view,
                update_ttl,
                notifications,
            }),
            remove_forward: RefCell::new(None),
            destroyed: Cell::new(false),
            listeners: RefCell::default(),
            next_listener_id: Cell::new(0),
            forwarding: Cell::new(0),
        });
        ViewInner::forward(&inner);
        MaterializedView(inner)
    }

    fn view(&self) -> Rc<ArrayView> {
        self.0.binding.borrow().view.clone()
    }

    pub(crate) fn downgrade(&self) -> WeakView {
        WeakView(Rc::downgrade(&self.0))
    }

    /// Reads `fresh`, the same query on a client that replaced this one's,
    /// from now on; the listeners hear its data at once. The view this one
    /// read before is destroyed with `fresh`.
    pub(crate) fn rebind(&self, fresh: MaterializedView) {
        if self.is_destroyed() {
            return;
        }
        for inner in [&self.0, &fresh.0] {
            if let Some(r) = inner.remove_forward.borrow_mut().take() {
                r();
            }
        }
        std::mem::swap(
            &mut *self.0.binding.borrow_mut(),
            &mut *fresh.0.binding.borrow_mut(),
        );
        ViewInner::forward(&self.0);
    }

    /// The view's rows now.
    pub fn data(&self) -> ViewValue {
        self.view().data()
    }

    /// Whether the server has confirmed the rows on this connection
    /// (`Complete`) or an earlier one (`Cached`), or not yet (`Unknown`), or
    /// the query failed (`Error`).
    pub fn result_type(&self) -> ResultType {
        self.view().listener_result_type().0
    }

    /// Why the query failed, when it did.
    pub fn error(&self) -> Option<ErroredQuery> {
        self.view().listener_result_type().1
    }

    /// Whether `destroy` was called.
    pub fn is_destroyed(&self) -> bool {
        self.0.destroyed.get()
    }

    /// The primary key columns of the rows.
    pub fn primary_key(&self) -> Vec<String> {
        self.view().schema().primary_key.clone()
    }

    /// Calls `listener` now and whenever the data or result type change.
    /// Returns the function that removes it.
    pub fn add_listener(
        &self,
        listener: impl Fn(&ViewValue, ResultType, Option<&ErroredQuery>) + 'static,
    ) -> Box<dyn Fn()> {
        let id = self.0.next_listener_id.get();
        self.0.next_listener_id.set(id + 1);
        let listener: ViewListener = Rc::new(listener);
        self.0.listeners.borrow_mut().push((id, listener.clone()));
        let view = self.view();
        let (ty, error) = view.listener_result_type();
        listener(&view.data(), ty, error.as_ref());
        let weak = Rc::downgrade(&self.0);
        Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.listeners.borrow_mut().retain(|(i, _)| *i != id);
            }
        })
    }

    /// Stops updating the view and releases its query, which the server
    /// keeps syncing for the query's TTL.
    pub fn destroy(&self) {
        self.0.destroy();
    }

    /// Raises the query's TTL; a lower one is ignored.
    pub fn update_ttl(&self, ttl: &Ttl) {
        (self.0.binding.borrow().update_ttl)(ttl);
    }
}

impl ViewInner {
    /// Passes what the bound view says on to the listeners.
    fn forward(inner: &Rc<ViewInner>) {
        let weak = Rc::downgrade(inner);
        let view = inner.binding.borrow().view.clone();
        let remove = view.add_listener(move |data, result_type| {
            let Some(inner) = weak.upgrade() else { return };
            let (error, notifications) = {
                let b = inner.binding.borrow();
                (b.view.listener_result_type().1, b.notifications.clone())
            };
            notifications.set(notifications.get() + 1);
            inner.forwarding.set(inner.forwarding.get() + 1);
            // Each listener in turn, reading the list again after every
            // call, as JS iterates a `Set` that changes meanwhile.
            let mut last: Option<u64> = None;
            loop {
                let next = inner
                    .listeners
                    .borrow()
                    .iter()
                    .find(|(id, _)| last.is_none_or(|l| *id > l))
                    .cloned();
                let Some((id, listener)) = next else { break };
                last = Some(id);
                listener(data, result_type, error.as_ref());
            }
            notifications.set(notifications.get() - 1);
            inner.forwarding.set(inner.forwarding.get() - 1);
            // Destroyed during the pass: its listeners went on hearing it.
            if inner.destroyed.get() && inner.forwarding.get() == 0 {
                inner.listeners.borrow_mut().clear();
            }
        });
        *inner.remove_forward.borrow_mut() = Some(remove);
    }

    fn destroy(&self) {
        if self.destroyed.replace(true) {
            return;
        }
        if let Some(r) = self.remove_forward.borrow_mut().take() {
            r();
        }
        if self.forwarding.get() == 0 {
            self.listeners.borrow_mut().clear();
        }
        let view = self.binding.borrow().view.clone();
        view.destroy();
    }
}

impl Drop for ViewInner {
    fn drop(&mut self) {
        self.destroy();
    }
}
