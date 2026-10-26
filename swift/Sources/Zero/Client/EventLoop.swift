/// Runs the engine's work in the order the JS client's event loop does.
///
/// The engine is synchronous. Where the JS client continues after a promise
/// settles, the engine queues a microtask. JS runs microtasks once the
/// current task's synchronous code is done and before the next task, so:
///
/// - Timers and socket events are tasks: they run in `dispatch`, which runs
///   the microtasks left by earlier code first and its own before it
///   returns.
/// - Calls from the app run in `call`, which leaves its microtasks for the
///   scheduler's next checkpoint, so that two calls in a row both happen
///   before either's follow-ups, as in JS.
///
/// Work nested in either runs in the outer one's drain.
@MainActor
final class EventLoop {
  private var queue: [() -> Void] = []
  private var head = 0
  private var afterTask: [() -> Void] = []
  private var depth = 0
  private var checkpointRequested = false
  private let scheduler: any Scheduler

  init(scheduler: any Scheduler) {
    self.scheduler = scheduler
  }

  private var stopped = false

  func microtask(_ task: @escaping () -> Void) {
    guard !stopped else { return }
    queue.append(task)
    if depth == 0 { requestCheckpoint() }
  }

  /// Runs `work` once the current task or call is done, before its
  /// microtasks. Outside a task or call (a microtask draining before a
  /// task's body), it is queued as a microtask, which runs once the current
  /// one is done.
  func afterCurrentTask(_ work: @escaping () -> Void) {
    guard !stopped else { return }
    if depth == 0 { return microtask(work) }
    afterTask.append(work)
  }

  private func runAfterTask() {
    var i = 0
    while i < afterTask.count {
      afterTask[i]()
      i += 1
    }
    afterTask.removeAll()
  }

  /// Runs a task: the microtasks already queued, `body`, then the
  /// microtasks it queued.
  func dispatch<T>(_ body: () throws -> T) rethrows -> T {
    if depth > 0 { return try body() }
    drain()
    depth = 1
    defer {
      runAfterTask()
      drain()
      depth = 0
    }
    return try body()
  }

  /// Runs a call from the app. Its microtasks run at the next checkpoint
  /// or task, whichever comes first.
  func call<T>(_ body: () throws -> T) rethrows -> T {
    if depth > 0 { return try body() }
    depth = 1
    defer {
      runAfterTask()
      depth = 0
      if head < queue.count { requestCheckpoint() }
    }
    return try body()
  }

  /// Drops the queued microtasks and every later one: the engine they
  /// continue is gone.
  func stop() {
    stopped = true
    queue.removeAll()
    afterTask.removeAll()
    head = 0
  }

  private func requestCheckpoint() {
    guard !checkpointRequested else { return }
    checkpointRequested = true
    scheduler.checkpoint { [weak self] in
      guard let self else { return }
      checkpointRequested = false
      dispatch {}
    }
  }

  private func drain() {
    while head < queue.count {
      let task = queue[head]
      head += 1
      task()
    }
    queue.removeAll(keepingCapacity: true)
    head = 0
  }
}

/// A scheduler whose timers fire inside the event loop's `dispatch`.
@MainActor
final class LoopScheduler: Scheduler {
  let base: any Scheduler
  let loop: EventLoop

  init(_ base: any Scheduler, _ loop: EventLoop) {
    self.base = base
    self.loop = loop
  }

  var now: Double { base.now }
  var monotonicNow: Double { base.monotonicNow }

  func schedule(
    after ms: Double, _ action: @escaping @MainActor () -> Void
  )
    -> any ScheduledTimer
  {
    base.schedule(after: ms) { [loop] in loop.dispatch(action) }
  }

  func scheduleRepeating(
    every ms: Double, _ action: @escaping @MainActor () -> Void
  )
    -> any ScheduledTimer
  {
    base.scheduleRepeating(every: ms) { [loop] in loop.dispatch(action) }
  }

  func checkpoint(_ action: @escaping @MainActor () -> Void) { base.checkpoint(action) }
}
