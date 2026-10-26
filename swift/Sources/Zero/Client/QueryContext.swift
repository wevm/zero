/// Builds pipelines over the sources of an IVM branch.
final class BranchDelegate: BuilderDelegate {
  let branch: IVMBranch
  init(_ branch: IVMBranch) { self.branch = branch }
  func getSource(_ tableName: String) -> (any Source)? { branch.source(tableName) }
}

/// Materializes queries over an IVM branch, registers named ones with the
/// query manager, and flushes views when a transaction commits
/// (`ZeroContext` in `packages/zero-client/src/client/context.ts`, with
/// `materializeImpl`, `runImpl`, and `preloadImpl` from
/// `packages/zql/src/query/query-delegate-base.ts`).
@MainActor
final class QueryContext {
  /// Builds a deferred pipeline; returns the release to call once every
  /// pending pipeline is built.
  typealias AttachPipeline = () throws -> () -> Void

  let branch: IVMBranch
  private let delegate: BranchDelegate
  /// Registers a named query with the server; returns its unregister.
  private let addCustomQuery: (AST, CustomQueryID, TTL, @escaping GotCallback) -> () -> Void
  private let updateCustomQuery: (CustomQueryID, TTL) -> Void
  private let batchViewUpdatesImpl: (() -> Void) -> Void
  private let microtask: (@escaping () -> Void) -> Void
  private var commitListeners: [(id: Int, listener: () -> Void)] = []
  private var nextListenerID = 0
  /// Until the replica is loaded into the sources, pipelines are built
  /// later, together (`markPipelinesReady`).
  private(set) var pipelinesReady = false
  private var pendingPipelines: [(id: Int, attach: AttachPipeline)] = []
  private let log: Log?
  /// Why the client closed, once it has.
  private let closedReason: () -> ConnectionError?
  /// The hashes of the unnamed queries already warned about.
  private var warnedLocalOnly = Set<String>()
  /// Counts the view notifications under way.
  let notifications = NotificationGate()

  /// Whether listeners are being told about a change. JS mutators are
  /// async, so a mutation made from a listener commits once the
  /// notification is over; `Engine.mutate` defers one made now.
  var isNotifying: Bool { notifications.depth > 0 }

  init(
    branch: IVMBranch,
    addCustomQuery: @escaping (AST, CustomQueryID, TTL, @escaping GotCallback) -> () -> Void,
    updateCustomQuery: @escaping (CustomQueryID, TTL) -> Void,
    batchViewUpdates: @escaping (() -> Void) -> Void,
    microtask: @escaping (@escaping () -> Void) -> Void,
    log: Log? = nil,
    closedReason: @escaping () -> ConnectionError? = { nil }
  ) {
    self.branch = branch
    self.delegate = BranchDelegate(branch)
    self.addCustomQuery = addCustomQuery
    self.updateCustomQuery = updateCustomQuery
    self.batchViewUpdatesImpl = batchViewUpdates
    self.microtask = microtask
    self.log = log
    self.closedReason = closedReason
  }

  /// The error of a query waiting for the server on a closed client: the
  /// one a query pending at close gets (`QueryManager.handleClosed`). JS
  /// only rejects the pending ones, so new ones never settle there.
  private func closedError(_ query: AnyQuery) -> QueryError? {
    guard let reason = closedReason() else { return nil }
    let id =
      query.customQueryID.map { QueryHash.hashOfNameAndArgs($0.name, $0.args) }
      ?? QueryHash.hashOfAST(query.ast)
    return QueryError(
      QueryManager.closedError(id: id, name: query.customQueryID?.name, reason))
  }

  /// Warns, once per query, that an unnamed query is waited for or
  /// preloaded: it only sees local data, so the server never confirms it.
  /// As in JS, where an unnamed query isn't sent to the server either
  /// (without `enableLegacyQueries`), it isn't an error.
  private func warnLocalOnly(_ query: AnyQuery, _ consequence: String) {
    guard query.customQueryID == nil, let log else { return }
    let hash = QueryHash.hashOfAST(query.ast)
    guard warnedLocalOnly.insert(hash).inserted else { return }
    log.warn(
      "The unnamed query of \"\(query.ast.table)\" (hash \(hash)) only sees local data and is "
        + "never complete: \(consequence). To sync it from the server, use a named query "
        + "(defineQuery, passed as a QueryRequest).")
  }

  /// A context for the queries a mutator runs: nothing is registered with
  /// the server, and the branch is already populated.
  static func local(_ branch: IVMBranch) -> QueryContext {
    let context = QueryContext(
      branch: branch, addCustomQuery: { _, _, _, _ in {} }, updateCustomQuery: { _, _ in },
      batchViewUpdates: { $0() }, microtask: { $0() })
    context.markPipelinesReady()
    return context
  }

  func batchViewUpdates(_ body: () -> Void) {
    var performed = false
    batchViewUpdatesImpl {
      body()
      performed = true
    }
    precondition(performed, "batchViewUpdates must call applyViewUpdates synchronously.")
  }

  func onTransactionCommit(_ listener: @escaping () -> Void) -> () -> Void {
    let id = nextListenerID
    nextListenerID += 1
    commitListeners.append((id, listener))
    return { [weak self] in self?.commitListeners.removeAll { $0.id == id } }
  }

  private func onPipelinesReady(_ attach: @escaping AttachPipeline) -> () -> Void {
    precondition(!pipelinesReady, "onPipelinesReady called while pipelines are ready")
    let id = nextListenerID
    nextListenerID += 1
    pendingPipelines.append((id, attach))
    return { [weak self] in self?.pendingPipelines.removeAll { $0.id == id } }
  }

  /// Builds and hydrates every deferred pipeline, in materialization order,
  /// as one batch of view updates.
  func markPipelinesReady() {
    if pipelinesReady { return }
    if pendingPipelines.isEmpty {
      pipelinesReady = true
      return
    }
    batchViewUpdates {
      var releases: [() -> Void] = []
      while !pendingPipelines.isEmpty {
        let (_, attach) = pendingPipelines.removeFirst()
        // A failing view has already been marked as errored; the others
        // are still built.
        if let release = try? attach() { releases.append(release) }
      }
      pipelinesReady = true
      for release in releases { release() }
      endTransaction()
    }
  }

  /// Why pushing into the branch failed, once it has (`#advanceError` in
  /// `ivm-branch.ts`). The pipelines saw part of that push, so they no
  /// longer match the sources: as in JS, the branch is unusable from then
  /// on, and every later push, `materialize`, and mutation fails with it.
  private(set) var branchFailure: PipelineError?

  /// Pushes `diff` into the branch and flushes the views. A push an
  /// operator fails (where JS throws from it) still flushes what the views
  /// got, as JS's `finally` does, and then throws.
  func processChanges(_ diff: KVDiff, rows: [String: Row] = [:]) throws {
    if let branchFailure { throw branchFailure }
    var failure: PipelineError?
    batchViewUpdates {
      do {
        try PipelineFailure.capture { branch.apply(diff, rows: rows) }
      } catch {
        failure = error as? PipelineError ?? PipelineError("\(error)")
      }
      endTransaction()
    }
    if let failure {
      branchFailure = failure
      throw failure
    }
  }

  private func endTransaction() {
    notifications.depth += 1
    defer { notifications.depth -= 1 }
    for (_, l) in commitListeners { l() }
  }

  /// The settled state of a view's query: still waiting, confirmed by the
  /// server, or failed.
  private enum Completion {
    case pending, complete, failed(ErroredQuery)
  }

  func materialize(_ query: AnyQuery, ttl: TTL = .defaultTTL) throws -> MaterializedView {
    if let message = query.invalid { throw PipelineError(message) }
    // JS's getSource throws once the branch failed. Unlike JS, the query
    // isn't registered first.
    if let branchFailure { throw branchFailure }
    let customQueryID = query.customQueryID
    let ast = query.ast
    let queryID =
      customQueryID.map { QueryHash.hashOfNameAndArgs($0.name, $0.args) }
      ?? QueryHash.hashOfAST(ast)
    let microtask = self.microtask
    let deferPipeline = !pipelinesReady
    var attached = !deferPipeline
    var got: Got = .no
    var queryComplete: Completion = .pending
    // `queryCompleteResolver`: the view hears about its settling one
    // microtask after it (two for a failure, as `.then().catch()` does).
    var resolved: Completion?
    var onResolved: ((Completion) -> Void)?
    let settle = { (r: Completion) in
      guard resolved == nil else { return }
      resolved = r
      onResolved?(r)
    }
    var view: ArrayView?
    var destroyed = false

    let maybeResolveComplete = {
      if attached, got == .yes, case .pending = queryComplete {
        queryComplete = .complete
        settle(.complete)
      }
    }
    let maybeMarkCached = {
      if attached, got == .cached, case .pending = queryComplete { view?.markCached() }
    }
    let gotCallback: GotCallback = { value, error in
      // Removals are deferred while mutations are pending, so a destroyed
      // view can still hear from the query manager.
      if destroyed { return }
      if let error {
        settle(.failed(error))
        queryComplete = .failed(error)
        return
      }
      got = value
      switch value {
      case .cached: maybeMarkCached()
      case .no: view?.unmarkCached()
      case .yes: maybeResolveComplete()
      }
    }

    var removeCommitObserver: (() -> Void)?
    var removePendingAttach: (() -> Void)?
    let removeAddedQuery =
      customQueryID.map { addCustomQuery(ast, $0, ttl, gotCallback) } ?? {}

    let input: any Input
    let deferred: DeferredInput?
    do {
      if deferPipeline {
        let delegate = self.delegate
        let build = { () throws -> any Input in try buildPipeline(ast, delegate, queryID: queryID) }
        // A probe gets the schema, and makes a query that cannot be built
        // throw now rather than later.
        let probe = try build()
        let schema = probe.getSchema()
        probe.destroy()
        deferred = DeferredInput(schema: schema, build: build)
        input = deferred!
      } else {
        deferred = nil
        input = try buildPipeline(ast, delegate, queryID: queryID)
      }
    } catch {
      removeAddedQuery()
      throw error
    }

    var created: ArrayView!
    var hydrationError: (any Error)?
    batchViewUpdates {
      let v: ArrayView
      // Hydrating fetches the pipeline, which can fail where JS throws.
      func hydrate(_ error: ErroredQuery?, complete: Bool) -> ArrayView? {
        do {
          return try PipelineFailure.capture {
            ArrayView(input, format: query.format, error: error, complete: complete)
          }
        } catch {
          hydrationError = error
          return nil
        }
      }
      switch queryComplete {
      case .complete:
        guard let h = hydrate(nil, complete: true) else { return }
        v = h
      case .failed(let e):
        guard let h = hydrate(e, complete: false) else { return }
        v = h
      case .pending:
        guard let h = hydrate(nil, complete: false) else { return }
        v = h
        onResolved = { [weak v] (r: Completion) in
          switch r {
          case .complete: microtask { v?.setResultType(.complete) }
          case .failed(let e): microtask { microtask { v?.setResultType(.error, error: e) } }
          case .pending: break
          }
        }
        if let resolved { onResolved!(resolved) }
      }
      removeCommitObserver = onTransactionCommit { [weak v] in v?.flush() }
      created = v
    }
    if let hydrationError {
      // Unlike JS, which leaves the query registered, nothing is left
      // behind: there is no view to destroy.
      destroyed = true
      input.destroy()
      removeAddedQuery()
      throw hydrationError
    }
    created.onDestroy = {
      destroyed = true
      removePendingAttach?()
      removePendingAttach = nil
      input.destroy()
      removeCommitObserver?()
      removeAddedQuery()
    }

    if let deferred {
      removePendingAttach = onPipelinesReady { [weak created] in
        removePendingAttach = nil
        if deferred.destroyed { return {} }
        do {
          created?.holdData()
          try deferred.attach()
        } catch {
          let e = ErroredQuery.app(
            id: queryID, name: customQueryID?.name ?? "", message: errorMessage(error),
            details: nil)
          queryComplete = .failed(e)
          settle(.failed(e))
          throw error
        }
        return {
          if destroyed { return }
          attached = true
          created?.releaseData()
          maybeResolveComplete()
          maybeMarkCached()
        }
      }
    }

    view = created
    maybeMarkCached()
    let updateTTL: (TTL) -> Void =
      customQueryID.map { id in { [weak self] ttl in self?.updateCustomQuery(id, ttl) } } ?? { _ in
      }
    return MaterializedView(created, updateTTL: updateTTL, notifications: notifications)
  }

  enum RunType: String, Sendable {
    case unknown, cached, complete
  }

  /// `runImpl`: the rows now, or once the server has confirmed them.
  func run(
    _ query: AnyQuery, type: RunType = .unknown, ttl: TTL? = nil
  ) throws -> Deferred<
    ViewValue
  > {
    let result = Deferred<ViewValue>()
    // Swift only: on a closed client, waiting for the server fails at once
    // rather than never settling, without materializing anything.
    if type != .unknown, let error = closedError(query) {
      result.reject(error)
      return result
    }
    let v: MaterializedView
    do {
      v = try materialize(query, ttl: ttl ?? .defaultTTL)
    } catch {
      // JS's `run` is async: a query that can't be materialized rejects it.
      result.reject(error)
      return result
    }
    if type == .unknown {
      // While the pipelines are not ready the view is deferred and this is
      // empty: `unknown` does not wait for local data either.
      let data = v.data
      v.destroy()
      result.resolve(data)
      return result
    }
    warnLocalOnly(
      query, "a run waiting for .\(type.rawValue) never returns unless its task is cancelled")
    // Settling releases the view, also when `Zero.run` rejects it because
    // its task was cancelled.
    result.onSettle { _ in v.destroy() }
    _ = v.addListener { data, resultType, error in
      if resultType == .complete || (type == .cached && resultType == .cached) {
        result.resolve(data)
      } else if resultType == .error {
        result.reject(QueryError(error, data: data))
      }
    }
    return result
  }

  /// The rows of `query` now, for a query run inside a mutator.
  func snapshot(_ query: AnyQuery) throws -> ViewValue {
    let v = try materialize(query)
    let data = v.data
    v.destroy()
    return data
  }

  /// `preloadImpl`: keeps a query's rows synced without a view.
  func preload(_ query: AnyQuery, ttl: TTL = .defaultPreloadTTL) -> Preload {
    let cached = Deferred<Void>()
    let complete = Deferred<Void>()
    // Swift only: on a closed client, both fail at once and nothing is
    // registered.
    if let error = closedError(query) {
      cached.reject(error)
      complete.reject(error)
      return Preload(cleanup: {}, cached: cached, complete: complete)
    }
    if let message = query.invalid {
      log?.error(message)
      cached.reject(PipelineError(message))
      complete.reject(PipelineError(message))
      return Preload(cleanup: {}, cached: cached, complete: complete)
    }
    guard let id = query.customQueryID else {
      warnLocalOnly(query, "preloading it does nothing, and its cached and complete never settle")
      return Preload(cleanup: {}, cached: cached, complete: complete)
    }
    let cleanup = addCustomQuery(query.ast, id, ttl) { got, error in
      if let error {
        cached.reject(QueryError(error))
        complete.reject(QueryError(error))
        return
      }
      if got == .yes {
        cached.resolve(())
        complete.resolve(())
      } else if got == .cached {
        cached.resolve(())
      }
    }
    return Preload(cleanup: cleanup, cached: cached, complete: complete)
  }
}

/// What `preload` returns.
@MainActor
public struct Preload {
  /// Stops syncing the query (after its TTL).
  public let cleanup: () -> Void
  /// Settles once the store holds a server-confirmed result, from this
  /// connection or an earlier one.
  public let cached: Deferred<Void>
  /// Settles once the server has confirmed the result on this connection.
  public let complete: Deferred<Void>
}

/// A query the server could not run.
public struct QueryError: Error, Sendable, CustomStringConvertible {
  public let error: ErroredQuery?
  /// What the view held when the query failed. JS rejects `run` with it.
  let data: ViewValue?

  package init(_ error: ErroredQuery?, data: ViewValue? = nil) {
    self.error = error
    self.data = data
  }

  public var description: String {
    switch error {
    case .app(_, _, let message, _)?: message ?? "Query failed"
    case .parse(_, _, let message, _)?: message
    case nil: "Query failed"
    }
  }
}

/// A view of a query that stays current as data changes (`TypedView`).
///
/// Unlike in JS, dropping the last reference to a view destroys it. When
/// `Zero` replaces its client (see `ZeroOptions.onClientStateNotFound`),
/// the view moves to the new client and keeps its identity and listeners.
@MainActor
public final class MaterializedView {
  public typealias Listener = (ViewValue, ResultType, ErroredQuery?) -> Void

  private(set) var view: ArrayView
  private var updateTTLImpl: (TTL) -> Void
  private var removeForward: () -> Void = {}
  private var destroyed = false
  private var listeners: [(id: Int, listener: Listener)] = []
  private var nextListenerID = 0
  /// What a new client materializes again: set by `Zero`.
  var query: AnyQuery?
  var ttl: TTL = .defaultTTL

  private var notifications: NotificationGate
  /// How many notification passes are under way. As in JS, a listener that
  /// destroys the view doesn't keep the pass from reaching the others.
  private var forwarding = 0

  init(
    _ view: ArrayView, updateTTL: @escaping (TTL) -> Void, notifications: NotificationGate
  ) {
    self.view = view
    self.updateTTLImpl = updateTTL
    self.notifications = notifications
    forward()
  }

  isolated deinit {
    destroy()
  }

  public var data: ViewValue { view.data }
  public var resultType: ResultType { view.listenerResultType.0 }
  public var error: ErroredQuery? { view.listenerResultType.1 }
  public var isDestroyed: Bool { destroyed }

  /// The primary key columns of the rows, which make their `RowID`s.
  public var primaryKey: [String] { view.input.getSchema().primaryKey }

  /// Calls `listener` now and whenever the data or result type change.
  @discardableResult
  public func addListener(_ listener: @escaping Listener) -> () -> Void {
    let id = nextListenerID
    nextListenerID += 1
    listeners.append((id, listener))
    let (type, error) = view.listenerResultType
    listener(view.data, type, error)
    return { [weak self] in self?.listeners.removeAll { $0.id == id } }
  }

  /// Calls `listener` now and whenever the data or result type change,
  /// with how the rows changed since its previous call.
  @discardableResult
  public func addSnapshotListener(_ listener: @escaping (ViewSnapshot) -> Void) -> () -> Void {
    var differ = RowDiffer(primaryKey: primaryKey)
    return addListener { data, type, error in listener(differ.next(data, type, error)) }
  }

  /// The view's snapshots: the current one, then one per change, until the
  /// iterating task is cancelled. The stream keeps the view alive.
  public var snapshots: AsyncStream<ViewSnapshot> {
    let (stream, continuation) = AsyncStream<ViewSnapshot>.makeStream()
    let subscription = SnapshotSubscription(self)
    subscription.remove = addSnapshotListener { continuation.yield($0) }
    continuation.onTermination = { _ in
      Task { @MainActor in subscription.cancel() }
    }
    return stream
  }

  private func forward() {
    removeForward = view.addListener { [weak self] data, type in
      guard let self else { return }
      let error = self.view.listenerResultType.1
      self.notifications.depth += 1
      self.forwarding += 1
      defer {
        self.notifications.depth -= 1
        self.forwarding -= 1
        // Destroyed during the pass: its listeners went on hearing it.
        if self.destroyed && self.forwarding == 0 { self.listeners = [] }
      }
      forEachLive(self.listeners) { listener in
        listener(data, type, error)
      } next: {
        [unowned self] in
        self.listeners
      }
    }
  }

  /// Takes over the pipeline of `other`, a view of the same query on a new
  /// client, and tells the listeners about its data.
  func rebind(to other: MaterializedView) {
    guard !destroyed else { return }
    removeForward()
    view.destroy()
    other.removeForward()
    other.destroyed = true
    view = other.view
    updateTTLImpl = other.updateTTLImpl
    notifications = other.notifications
    forward()
  }

  /// Stops updating the view and releases its query, which the server
  /// keeps syncing for the query's TTL.
  public func destroy() {
    guard !destroyed else { return }
    destroyed = true
    removeForward()
    if forwarding == 0 { listeners = [] }
    view.destroy()
  }

  /// Raises the query's TTL; a lower one is ignored.
  public func updateTTL(_ ttl: TTL) {
    self.ttl = ttl
    updateTTLImpl(ttl)
  }
}

/// How many view notifications are under way on a client.
@MainActor
final class NotificationGate {
  var depth = 0
}

/// Keeps a view alive while its snapshot stream is iterated.
@MainActor
private final class SnapshotSubscription {
  let view: MaterializedView
  var remove: (() -> Void)?

  init(_ view: MaterializedView) { self.view = view }

  func cancel() {
    remove?()
    remove = nil
  }
}
