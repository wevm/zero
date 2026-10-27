#if canImport(SwiftUI)
  import SwiftUI

  extension EnvironmentValues {
    /// The client that `@ZeroQuery` reads from (`ZeroProvider`). Set it
    /// with `.zero(_:)`.
    public var zero: (any AnyZero)? {
      get { self[ZeroKey.self] }
      set { self[ZeroKey.self] = newValue }
    }
  }

  private struct ZeroKey: EnvironmentKey {
    static let defaultValue: (any AnyZero)? = nil
  }

  extension View {
    /// Makes `zero` the client of this view hierarchy's `@ZeroQuery`s.
    public func zero(_ zero: some AnyZero) -> some View {
      environment(\.zero, zero)
    }
  }

  /// The rows of a query, kept current (`useQuery` in `zero-react`):
  ///
  /// ```swift
  /// @ZeroQuery(IssueQueries.open()) var issues
  ///
  /// var body: some View {
  ///   List(issues) { IssueRow(issue: $0) }
  ///     .overlay { if $issues.resultType == .unknown { ProgressView() } }
  /// }
  /// ```
  ///
  /// The client comes from the environment (`.zero(_:)`); its context type
  /// must be the request's. Until there is one, the value is empty.
  @MainActor
  @propertyWrapper
  public struct ZeroQuery<Row: ZeroRow, Result: QueryResult, Context: Sendable>:
    @preconcurrency DynamicProperty
  where Result.Row == Row {
    @Environment(\.zero) private var zero
    @State private var state = QueryState<Result>()
    private let request: QueryRequest<Row, Result, Context>
    private let ttl: TTL

    public init(_ request: QueryRequest<Row, Result, Context>, ttl: TTL = .defaultTTL) {
      self.request = request
      self.ttl = ttl
    }

    public var wrappedValue: Result { state.live?.data ?? state.empty }

    /// The query's result type and error.
    public var projectedValue: QueryState<Result> { state }

    public func update() {
      state.update(zero as? Zero<Context>, request, ttl)
    }
  }

  /// What `@ZeroQuery` knows about its query besides the rows.
  @Observable
  @MainActor
  public final class QueryState<Result: QueryResult> {
    /// The live query, once the environment has a client.
    public private(set) var live: LiveQuery<Result>?
    private var materializeError: (any Error)?
    @ObservationIgnored private var key: (name: String, args: [JSONValue], ttl: TTL)?
    /// The client `key` is of. Weak, and compared by identity: a client made after it was freed can have its address, and must not pass for it.
    @ObservationIgnored private weak var keyZero: AnyObject?
    @ObservationIgnored let empty: Result

    init() {
      // An array of no rows, or no row.
      empty = try! Result(view: .many([]))
    }

    public var resultType: ResultType { live?.resultType ?? .unknown }
    /// The query could not be built or run, or its rows not read.
    public var error: (any Error)? { materializeError ?? live?.error }

    func update<R, Context>(
      _ zero: Zero<Context>?, _ request: QueryRequest<R, Result, Context>, _ ttl: TTL
    ) {
      guard let zero else {
        key = nil
        keyZero = nil
        live = nil
        return
      }
      if let key, keyZero === zero, key.name == request.name, key.args == request.args,
        key.ttl == ttl
      {
        return
      }
      key = (request.name, request.args, ttl)
      keyZero = zero
      do {
        live = try zero.liveQuery(request, ttl: ttl)
        materializeError = nil
      } catch {
        live = nil
        materializeError = error
      }
    }
  }
#endif
