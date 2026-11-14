/// Times the pushes and reconciles of a source connection for the query's
/// `query-update-client` metric (`MeasurePushOperator` in
/// `packages/zql/src/query/measure-push-operator.ts`).
///
/// With a `sampleRate` of n, only the first of every n pushes (and
/// reconciles) is timed and reported; the metric weights each time by n.
final class MeasurePush: Operator {
  let input: any Input
  private let queryID: String
  private let sampleRate: Int
  private let now: () -> Double
  private let report: (Double, String) -> Void
  /// Pushes and reconciles so far, sampled or not.
  private var count = 0
  weak var output: (any Output)?

  init(
    _ input: any Input, queryID: String, sampleRate: Int, now: @escaping () -> Double,
    report: @escaping (Double, String) -> Void
  ) {
    self.input = input
    self.queryID = queryID
    self.sampleRate = sampleRate
    self.now = now
    self.report = report
    input.setOutput(self)
  }

  func setOutput(_ output: any Output) { self.output = output }
  func getSchema() -> SourceSchema { input.getSchema() }
  func destroy() { input.destroy() }
  func fetch(_ req: FetchRequest) -> NodeStream { input.fetch(req) }

  func push(_ change: Change, pusher: any InputBase) {
    guard let output else { return }
    measure(sample()) { output.push(change, pusher: self) }
  }

  func reconcile(pusher: any InputBase) {
    // A view has no `reconcile` in JS, which then neither counts nor times one.
    guard let output, !(output is ArrayView) else { return }
    measure(sample()) { output.reconcile(pusher: self) }
  }

  private func sample() -> Bool {
    defer { count += 1 }
    return count % sampleRate == 0
  }

  private func measure(_ sampled: Bool, _ body: () -> Void) {
    guard sampled else { return body() }
    let start = now()
    body()
    report(now() - start, queryID)
  }
}
