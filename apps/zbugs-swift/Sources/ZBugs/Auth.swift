import Zero

/// Who the client acts for: the `sub` and `role` of the zbugs JWT
/// (`AuthData` in apps/zbugs/shared/auth.ts). The context of every query
/// and mutator; nil when logged out.
public struct AuthData: Codable, Sendable, Hashable {
  public var sub: String
  public var role: UserRole

  public init(sub: String, role: UserRole) {
    self.sub = sub
    self.role = role
  }
}

public typealias Role = UserRole

/// apps/zbugs/shared/error.ts.
public enum MutationErrorCode: String, Sendable {
  case notLoggedIn = "NOT_LOGGED_IN"
  case notAuthorized = "NOT_AUTHORIZED"
  case notificationFailed = "NOTIFICATION_FAILED"
  case entityNotFound = "ENTITY_NOT_FOUND"
}

/// An `ApplicationError` with zbugs' details: `{code, id}`.
func mutationError(_ message: String, _ code: MutationErrorCode, _ id: String? = nil)
  -> ApplicationError
{
  var details: [String: JSONValue] = ["code": .string(code.rawValue)]
  if let id { details["id"] = .string(id) }
  return ApplicationError(message, details: .object(details))
}

/// `must` in packages/shared: the value, or the error JS throws for a
/// missing one.
func must<T>(_ value: T?) throws -> T {
  guard let value else { throw ApplicationError("Unexpected undefined value") }
  return value
}

func assertIsLoggedIn(_ auth: AuthData?) throws -> AuthData {
  guard let auth else {
    throw mutationError("User must be logged in for this operation", .notLoggedIn)
  }
  return auth
}

func isAdmin(_ auth: AuthData?) throws -> Bool {
  try assertIsLoggedIn(auth).role == .crew
}

/// A row with a creator: an issue, a comment, or an emoji.
protocol Created: ZeroRow {
  var creatorID: String { get }
  var id: String { get }
}

extension Issue: Created {}
extension Comment: Created {}
extension Emoji: Created {}

@MainActor
func assertIsCreatorOrAdmin<R: Created>(
  _ tx: Transaction, _ auth: AuthData?, _ query: Query<R, [R]>, _ id: String
) throws {
  let auth = try assertIsLoggedIn(auth)
  if try isAdmin(auth) { return }
  // By name: a key path through `Created` is not the row's own `\.id`.
  let entity = try tx.run(Query<R, [R]>(query.any.where("id", .string(id))).one())
  // Security: the same message whether or not the entity exists.
  guard let entity, auth.sub == entity.creatorID else {
    throw mutationError("Not authorized to access this resource", .notAuthorized, id)
  }
}

@MainActor
func assertUserCanSeeIssue(_ tx: Transaction, _ userID: String, _ issueID: String) throws {
  let issue = try must(tx.run(zql.issue.where(\.id, issueID).one()))
  let user = try must(tx.run(zql.user.where(\.id, userID).one()))
  if issue.visibility != .public && userID != issue.creatorID && user.role != .crew {
    throw mutationError(
      "User does not have permission to view this issue", .notAuthorized, issueID)
  }
}

@MainActor
func assertUserCanSeeComment(_ tx: Transaction, _ userID: String, _ commentID: String) throws {
  let comment = try must(tx.run(zql.comment.where(\.id, commentID).one()))
  try assertUserCanSeeIssue(tx, userID, comment.issueID)
}
