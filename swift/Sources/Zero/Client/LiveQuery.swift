import Observation

/// A query's rows as an observable object, typed by the query's result
/// (`useQuery` in `zero-react`). SwiftUI views, and UIKit and AppKit updates,
/// that read `data`, `resultType`, or `error` follow their changes.
///
/// The query stays synced while the object lives. When `Zero` replaces its
/// client, the object moves to the new one and keeps its identity.
@Observable
@MainActor
public final class LiveQuery<Result: QueryValue> {
  /// The rows, the row of a `one()` query, or the value of an aggregate.
  public private(set) var data: Result
  public private(set) var resultType: ResultType
  /// The server could not run the query (a `QueryError`), or the rows could
  /// not be read as `Result`. `data` keeps the last rows read.
  public private(set) var error: (any Error)?
  /// The view the object reads.
  @ObservationIgnored public let view: MaterializedView
  @ObservationIgnored private var removeListener: () -> Void = {}
  /// Reads the view's value as `Result`.
  @ObservationIgnored private let read: (ViewValue) throws -> Result

  init(_ view: MaterializedView) throws {
    self.view = view
    read = Self.reader()
    data = try read(view.data)
    resultType = view.resultType
    error = view.error.map { QueryError($0) }
    removeListener = view.addListener { [weak self] data, type, error in
      self?.update(data, type, error)
    }
  }

  isolated deinit {
    removeListener()
  }

  private func update(_ value: ViewValue, _ type: ResultType, _ queryError: ErroredQuery?) {
    resultType = type
    do {
      data = try read(value)
      error = queryError.map { QueryError($0) }
    } catch {
      self.error = error
    }
  }

  /// Rows are read through a `RowCache`; other values as they are.
  private static func reader() -> (ViewValue) throws -> Result {
    guard let rows = Result.self as? any QueryResult.Type else { return { try Result(view: $0) } }
    return rowsReader(rows)
  }

  private static func rowsReader<R: QueryResult>(_: R.Type) -> (ViewValue) throws -> Result {
    let cache = RowCache<R.Row>()
    // `R` is `Result`.
    return { try R(rows: cache.read($0)) as! Result }
  }

  /// Raises the query's TTL; a lower one is ignored.
  public func updateTTL(_ ttl: TTL) { view.updateTTL(ttl) }
}

/// The entries last read and the rows read from them: an update reads only
/// the rows whose entries changed.
@MainActor
private final class RowCache<Row: ZeroRow> {
  private var entries: [ViewEntry] = []
  private var rows: [Row] = []

  /// The rows of `value`, reusing the rows read from equal entries. Most
  /// updates change a few rows in place, or insert or remove one, so the
  /// entries are matched in order, skipping at most one.
  func read(_ value: ViewValue) throws(ZeroValueError) -> [Row] {
    let new = value.entries
    var out: [Row] = []
    out.reserveCapacity(new.count)
    var j = 0
    for entry in new {
      if j < entries.count, entries[j] == entry {
        out.append(rows[j])
        j += 1
      } else if j + 1 < entries.count, entries[j + 1] == entry {
        out.append(rows[j + 1])
        j += 2
      } else {
        out.append(try Row(entry: entry))
      }
    }
    entries = new
    rows = out
    return out
  }
}

extension Zero {
  /// The query's rows, or its aggregate, as an observable object that stays
  /// current.
  public func liveQuery<R: ZeroRow, S: QueryValue>(
    _ request: QueryRequest<R, S, Context>, ttl: TTL = .defaultTTL
  ) throws -> LiveQuery<S> {
    try LiveQuery(materialize(request, ttl: ttl))
  }

  /// The rows of an unnamed query, which only sees local data, as an
  /// observable object.
  public func liveQuery<R: ZeroRow, S: QueryValue>(
    _ query: Query<R, S>, ttl: TTL = .defaultTTL
  ) throws -> LiveQuery<S> {
    try LiveQuery(materialize(query, ttl: ttl))
  }
}
