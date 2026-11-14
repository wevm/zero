//! Who the client acts for, and the checks zbugs' mutators make
//! (`apps/zbugs/shared/auth.ts` and `error.ts`).

use crate::schema::{Comment, Emoji, Issue, User, UserRole, zql};
use zero_client::client::{ApplicationError, Error, Transaction};
use zero_client::json::{self, Object, Value};
use zero_client::typed::{Query, QueryElement, ZeroRow, ZeroValue};

/// The `sub` and `role` of a zbugs JWT (`AuthData`): the context of every
/// query and mutator, `None` when logged out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthData {
    pub sub: String,
    pub role: UserRole,
}

impl AuthData {
    /// The claims of a zbugs JWT, without checking its signature (the API
    /// server checks it): `None` when `jwt` isn't one, or has expired by
    /// `now` (seconds since 1970).
    pub fn from_jwt(jwt: &str, now: f64) -> Option<AuthData> {
        let parts: Vec<&str> = jwt.split('.').collect();
        if parts.len() != 3 {
            return None;
        }
        let payload = base64url_decode(parts[1])?;
        let claims = json::parse(std::str::from_utf8(&payload).ok()?).ok()?;
        let auth = AuthData::from_json(&claims)?;
        if let Some(exp) = claims.get("exp").and_then(Value::as_f64)
            && exp < now
        {
            return None;
        }
        Some(auth)
    }

    /// `{sub, role}`, as a drift sample's context holds it.
    pub fn from_json(value: &Value) -> Option<AuthData> {
        let sub = value.get("sub")?.as_str()?.to_owned();
        let role = UserRole::from_zero_value(value.get("role")?).ok()?;
        Some(AuthData { sub, role })
    }

    pub fn is_crew(&self) -> bool {
        self.role == UserRole::Crew
    }
}

/// RFC 4648 §5 base64url, padded or not.
fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    let mut bits: u32 = 0;
    let mut count = 0;
    let mut out = Vec::new();
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => continue,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(v);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
        }
    }
    Some(out)
}

/// `apps/zbugs/shared/error.ts`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationErrorCode {
    NotLoggedIn,
    NotAuthorized,
    NotificationFailed,
    EntityNotFound,
}

impl MutationErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            MutationErrorCode::NotLoggedIn => "NOT_LOGGED_IN",
            MutationErrorCode::NotAuthorized => "NOT_AUTHORIZED",
            MutationErrorCode::NotificationFailed => "NOTIFICATION_FAILED",
            MutationErrorCode::EntityNotFound => "ENTITY_NOT_FOUND",
        }
    }
}

/// An `ApplicationError` with zbugs' details: `{code, id}`.
pub(crate) fn mutation_error(message: &str, code: MutationErrorCode, id: Option<&str>) -> Error {
    let mut details = Object::new();
    details.insert("code", Value::from(code.as_str()));
    if let Some(id) = id {
        details.insert("id", Value::from(id));
    }
    ApplicationError::with_details(message, Some(Value::Object(details))).into()
}

/// `must` in packages/shared: the value, or the error JS throws for a
/// missing one.
pub(crate) fn must<T>(value: Option<T>) -> Result<T, Error> {
    value.ok_or_else(|| ApplicationError::new("Unexpected undefined value").into())
}

pub(crate) fn assert_is_logged_in(auth: &Option<AuthData>) -> Result<&AuthData, Error> {
    auth.as_ref().ok_or_else(|| {
        mutation_error(
            "User must be logged in for this operation",
            MutationErrorCode::NotLoggedIn,
            None,
        )
    })
}

pub(crate) fn is_admin(auth: &Option<AuthData>) -> Result<bool, Error> {
    Ok(assert_is_logged_in(auth)?.is_crew())
}

/// Runs `query` in the mutator's transaction and reads it as its result.
pub(crate) fn run<R, T: zero_client::typed::QueryValue>(
    tx: &Transaction,
    query: Query<R, T>,
) -> Result<T, Error> {
    let view = tx.run(query.any())?;
    query
        .read(&view)
        .map_err(|e| ApplicationError::new(e.0).into())
}

/// A row with a creator: an issue, a comment, or an emoji.
pub(crate) trait Created: ZeroRow + QueryElement {
    fn creator_id(&self) -> &str;
}

impl Created for Issue {
    fn creator_id(&self) -> &str {
        &self.creator_id
    }
}

impl Created for Comment {
    fn creator_id(&self) -> &str {
        &self.creator_id
    }
}

impl Created for Emoji {
    fn creator_id(&self) -> &str {
        &self.creator_id
    }
}

pub(crate) fn assert_is_creator_or_admin<R: Created + zero_client::typed::OptionalQueryElement>(
    tx: &Transaction,
    auth: &Option<AuthData>,
    query: Query<R>,
    id: &str,
) -> Result<(), Error> {
    let auth = assert_is_logged_in(auth)?;
    if auth.is_crew() {
        return Ok(());
    }
    let q = Query::<R>::from_any(query.into_any().where_(
        "id",
        zero_client::typed::SimpleOperator::Eq,
        Value::from(id),
    ))
    .one();
    let entity = run(tx, q)?;
    // Security: the same message whether or not the entity exists.
    match entity {
        Some(e) if e.creator_id() == auth.sub => Ok(()),
        _ => Err(mutation_error(
            "Not authorized to access this resource",
            MutationErrorCode::NotAuthorized,
            Some(id),
        )),
    }
}

pub(crate) fn assert_user_can_see_issue(
    tx: &Transaction,
    user_id: &str,
    issue_id: &str,
) -> Result<(), Error> {
    let issue = must(run(
        tx,
        zql::issue().where_eq(Issue::ID, issue_id.to_owned()).one(),
    )?)?;
    let user: User = must(run(
        tx,
        zql::user().where_eq(User::ID, user_id.to_owned()).one(),
    )?)?;
    if issue.visibility != crate::schema::IssueVisibility::Public
        && user_id != issue.creator_id
        && user.role != UserRole::Crew
    {
        return Err(mutation_error(
            "User does not have permission to view this issue",
            MutationErrorCode::NotAuthorized,
            Some(issue_id),
        ));
    }
    Ok(())
}

pub(crate) fn assert_user_can_see_comment(
    tx: &Transaction,
    user_id: &str,
    comment_id: &str,
) -> Result<(), Error> {
    let comment = must(run(
        tx,
        zql::comment()
            .where_eq(Comment::ID, comment_id.to_owned())
            .one(),
    )?)?;
    assert_user_can_see_issue(tx, user_id, &comment.issue_id)
}

/// A new row ID, as zbugs makes them (`nanoid`): 21 URL-safe characters.
pub fn make_id() -> String {
    const ALPHABET: &[u8; 64] = b"useandom-26T198340PX75pxJACKVERYMINDBUSHWOLF_GQZbfghjklqvwyzrict";
    let mut bytes = [0u8; 21];
    getrandom::fill(&mut bytes).expect("the system's random numbers");
    bytes
        .iter()
        .map(|b| ALPHABET[usize::from(b & 63)] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // {"sub":"u-1","role":"crew","exp":2000000000}
    const TOKEN: &str =
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ1LTEiLCJyb2xlIjoiY3JldyIsImV4cCI6MjAwMDAwMDAwMH0.sig";

    #[test]
    fn reads_a_current_token_and_refuses_an_expired_one() {
        assert_eq!(
            AuthData::from_jwt(TOKEN, 1_900_000_000.0),
            Some(AuthData {
                sub: "u-1".into(),
                role: UserRole::Crew
            })
        );
        assert_eq!(AuthData::from_jwt(TOKEN, 2_000_000_001.0), None);
        assert_eq!(AuthData::from_jwt("not.a.jwt!", 0.0), None);
        assert_eq!(AuthData::from_jwt("abc", 0.0), None);
    }

    #[test]
    fn makes_distinct_ids_of_21_url_safe_characters() {
        let (a, b) = (make_id(), make_id());
        assert_eq!(a.len(), 21);
        assert_ne!(a, b);
        assert!(
            a.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        );
    }
}
