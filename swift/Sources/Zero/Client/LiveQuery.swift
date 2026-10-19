import Observation

/// A query's rows as an observable object, typed by the query's result
/// (`useQuery` in `zero-react`). SwiftUI views, and UIKit and AppKit updates,
/// that read `data`, `resultType`, or `error` follow their changes.
///
/// The query stays synced while the object lives. When `Zero` replaces its
/// client, the object moves to the new one and keeps its identity.
@Observable
@MainActor
public final class LiveQuery<Result: QueryResult> {
  /// The rows, or the row of a `one()` query.
  public private(set) var data: Result
  public private(set) var resultType: ResultType
  /// The server could not run the query (a `QueryError`), or the rows could
  /// not be read as `Result`. `data` keeps the last rows read.
  public private(set) var error: (any Error)?
  /// The view the object reads.
  @ObservationIgnored public let view: MaterializedView
  @ObservationIgnored private var removeListener: () -> Void = {}

  init(_ view: MaterializedView) throws {
    self.view = view
    data = try Result(view: view.data)
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
      data = try Result(view: value)
      error = queryError.map { QueryError($0) }
    } catch {
      self.error = error
    }
  }

  /// Raises the query's TTL; a lower one is ignored.
  public func updateTTL(_ ttl: TTL) { view.updateTTL(ttl) }
}

extension Zero {
  /// The query's rows as an observable object that stays current.
  public func liveQuery<R: ZeroRow, S: QueryResult>(
    _ request: QueryRequest<R, S, Context>, ttl: TTL = .defaultTTL
  ) throws -> LiveQuery<S> {
    try LiveQuery(materialize(request, ttl: ttl))
  }

  /// The rows of an unnamed query, which only sees local data, as an
  /// observable object.
  public func liveQuery<R: ZeroRow, S: QueryResult>(
    _ query: Query<R, S>, ttl: TTL = .defaultTTL
  ) throws -> LiveQuery<S> {
    try LiveQuery(materialize(query, ttl: ttl))
  }
}
