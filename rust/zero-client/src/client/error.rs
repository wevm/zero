//! The errors of the client: why a connection ended, an error a mutator or
//! the API server raised, and a query the server could not run.

use crate::ivm::{PipelineError, ViewValue};
use crate::json::Value;
use crate::keys::InvalidPrimaryKeyValue;
use crate::protocol::{ErrorBody, ErrorKind, ErrorOrigin, ErroredQuery, FailureCause};
use std::fmt;

crate::ast::string_enum!(
    /// The kinds of error the client raises itself
    /// (`packages/zero-client/src/client/client-error-kind-enum.ts`).
    ClientErrorKind {
        AbruptClose = "AbruptClose",
        CleanClose = "CleanClose",
        ClientClosed = "ClientClosed",
        ConnectTimeout = "ConnectTimeout",
        Hidden = "Hidden",
        Internal = "Internal",
        InvalidMessage = "InvalidMessage",
        NoSocketOrigin = "NoSocketOrigin",
        Offline = "Offline",
        PingTimeout = "PingTimeout",
        PullTimeout = "PullTimeout",
        UnexpectedBaseCookie = "UnexpectedBaseCookie",
        UserDisconnect = "UserDisconnect",
    }
);

/// Why a connection ended or could not be made: an error the client raised
/// (`ClientError`) or one the server sent (`ProtocolError`), together
/// `ZeroError` in `packages/zero-client/src/client/error.ts`.
#[derive(Clone, Debug, PartialEq)]
pub enum ConnectionError {
    /// An error the client raised, of a kind, with its message.
    Client(ClientErrorKind, String),
    /// An error the server sent.
    Server(Box<ErrorBody>),
}

impl ConnectionError {
    /// An error the client raised.
    pub fn client(kind: ClientErrorKind, message: impl Into<String>) -> Self {
        ConnectionError::Client(kind, message.into())
    }

    /// The error's message.
    pub fn message(&self) -> &str {
        match self {
            ConnectionError::Client(_, m) => m,
            ConnectionError::Server(body) => body.message(),
        }
    }

    /// The kind's name, a `ClientErrorKind` or an `ErrorKind`.
    pub fn kind_name(&self) -> &'static str {
        match self {
            ConnectionError::Client(kind, _) => kind.as_str(),
            ConnectionError::Server(body) => body.kind().as_str(),
        }
    }

    /// The kind of an error the client raised.
    pub fn client_kind(&self) -> Option<ClientErrorKind> {
        match self {
            ConnectionError::Client(kind, _) => Some(*kind),
            ConnectionError::Server(_) => None,
        }
    }

    /// The kind of an error the server sent.
    pub fn server_kind(&self) -> Option<ErrorKind> {
        match self {
            ConnectionError::Server(body) => Some(body.kind()),
            ConnectionError::Client(..) => None,
        }
    }

    /// `isZeroError`: a client error, or a server error that says where it
    /// came from. Older servers send errors without an origin.
    pub fn is_zero_error(&self) -> bool {
        match self {
            ConnectionError::Client(..) => true,
            ConnectionError::Server(body) => matches!(
                body.origin(),
                Some(ErrorOrigin::Server | ErrorOrigin::ZeroCache)
            ),
        }
    }

    /// `isAuthError`: the server rejected the credentials, or the API server
    /// answered a push or transform with 401 or 403.
    pub fn is_auth_error(&self) -> bool {
        let ConnectionError::Server(body) = self else {
            return false;
        };
        if !self.is_zero_error() {
            return false;
        }
        match &**body {
            ErrorBody::Basic { kind, .. } => {
                matches!(kind, ErrorKind::AuthInvalidated | ErrorKind::Unauthorized)
            }
            ErrorBody::PushFailed { cause, .. } | ErrorBody::TransformFailed { cause, .. } => {
                is_http_auth_failure(cause)
            }
            ErrorBody::Backoff { .. } => false,
        }
    }

    /// `isTransportFailure`: the socket closed or never answered, rather
    /// than the server replying. These carry no backoff instructions and
    /// usually clear within seconds, so they are retried on the fast
    /// schedule.
    pub fn is_transport_failure(&self) -> bool {
        matches!(
            self.client_kind(),
            Some(
                ClientErrorKind::AbruptClose
                    | ClientErrorKind::CleanClose
                    | ClientErrorKind::ConnectTimeout
                    | ClientErrorKind::PingTimeout
            )
        )
    }

    /// How a rejected mutation describes the client (`getStateDescription`).
    pub fn state_description(&self) -> &'static str {
        match self.client_kind() {
            Some(ClientErrorKind::Offline) => "offline",
            Some(ClientErrorKind::ClientClosed) => "closed",
            _ => "in error state",
        }
    }

    /// `getBackoffParams`.
    pub fn backoff(&self) -> Option<&ErrorBody> {
        match self {
            ConnectionError::Server(body)
                if matches!(**body, ErrorBody::Backoff { .. }) && self.is_zero_error() =>
            {
                Some(body)
            }
            _ => None,
        }
    }
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind_name(), self.message())
    }
}

impl std::error::Error for ConnectionError {}

fn is_http_auth_failure(cause: &FailureCause) -> bool {
    matches!(cause, FailureCause::Http { status, .. } if *status == 401.0 || *status == 403.0)
}

/// What a connection error does to the connection status
/// (`getErrorConnectionTransition`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectionTransition {
    /// Keep the status and retry.
    None,
    NeedsAuth,
    Error,
    Disconnected,
    Closed,
}

impl ConnectionTransition {
    /// The transition and the reason to record. An error without an origin
    /// is unexpected: it becomes an internal client error.
    pub(crate) fn of(error: &ConnectionError) -> (ConnectionTransition, ConnectionError) {
        if !error.is_zero_error() {
            return (
                ConnectionTransition::Error,
                ConnectionError::client(
                    ClientErrorKind::Internal,
                    format!("Unexpected internal error: {}", error.message()),
                ),
            );
        }
        (ConnectionTransition::new(error), error.clone())
    }

    fn new(error: &ConnectionError) -> ConnectionTransition {
        use ClientErrorKind as C;
        use ErrorKind as S;
        if error.is_auth_error() {
            return ConnectionTransition::NeedsAuth;
        }
        match error {
            ConnectionError::Client(kind, _) => match kind {
                C::AbruptClose
                | C::CleanClose
                | C::ConnectTimeout
                | C::PingTimeout
                | C::PullTimeout
                | C::UnexpectedBaseCookie => ConnectionTransition::None,
                C::Internal | C::InvalidMessage | C::UserDisconnect => ConnectionTransition::Error,
                C::Hidden | C::Offline | C::NoSocketOrigin => ConnectionTransition::Disconnected,
                C::ClientClosed => ConnectionTransition::Closed,
            },
            ConnectionError::Server(body) => match body.kind() {
                S::ClientNotFound
                | S::InvalidConnectionRequest
                | S::InvalidConnectionRequestBaseCookie
                | S::InvalidConnectionRequestLastMutationID
                | S::InvalidConnectionRequestClientDeleted
                | S::InvalidMessage
                | S::InvalidPush
                | S::VersionNotSupported
                | S::SchemaVersionNotSupported
                | S::Internal
                | S::PushFailed
                | S::TransformFailed => ConnectionTransition::Error,
                S::Rebalance | S::Rehome | S::ServerOverloaded => ConnectionTransition::None,
                S::AuthInvalidated | S::Unauthorized => ConnectionTransition::NeedsAuth,
                S::MutationRateLimited | S::MutationFailed => ConnectionTransition::None,
            },
        }
    }
}

/// An error a mutator raised for the app to handle, or that the API server
/// reported for a mutation (`ApplicationError` in
/// `packages/zero-protocol/src/application-error.ts`). Return it from a
/// mutator to give the caller a message and details.
#[derive(Clone, Debug, PartialEq)]
pub struct ApplicationError {
    /// What the app shows.
    pub message: String,
    /// Anything else the app wants to pass along, as JSON.
    pub details: Option<Value>,
}

impl ApplicationError {
    /// An error with `message` and no details.
    pub fn new(message: impl Into<String>) -> Self {
        ApplicationError {
            message: message.into(),
            details: None,
        }
    }

    /// With `details`; a null is no details (`details ?? undefined` in JS).
    pub fn with_details(message: impl Into<String>, details: Option<Value>) -> Self {
        ApplicationError {
            message: message.into(),
            details: details.filter(|d| !d.is_null()),
        }
    }

    /// `wrapWithApplicationError`.
    pub(crate) fn wrapping(error: &Error) -> ApplicationError {
        match error {
            Error::Application(e) => e.clone(),
            // `getErrorDetails` gives an error of another class its name.
            Error::InvalidPrimaryKey(e) => ApplicationError::with_details(
                e.to_string(),
                Some(Value::Object(
                    [("name", Value::from("TypeError"))].into_iter().collect(),
                )),
            ),
            e => ApplicationError::new(e.message()),
        }
    }
}

impl fmt::Display for ApplicationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApplicationError {}

/// A query the server could not run.
#[derive(Clone, Debug)]
pub struct QueryError {
    /// Why the server could not run the query, if it said.
    pub error: Option<ErroredQuery>,
    /// What the view held when the query failed. JS rejects `run` with it.
    pub data: Option<ViewValue>,
}

impl QueryError {
    /// A query error with no view data.
    pub fn new(error: Option<ErroredQuery>) -> Self {
        QueryError { error, data: None }
    }
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.error {
            Some(ErroredQuery::App {
                message: Some(m), ..
            }) => f.write_str(m),
            Some(ErroredQuery::Parse { message, .. }) => f.write_str(message),
            _ => f.write_str("Query failed"),
        }
    }
}

impl std::error::Error for QueryError {}

/// An error of the client's API.
#[derive(Clone, Debug)]
pub enum Error {
    /// A mutator or the API server raised it for the app.
    Application(ApplicationError),
    /// The connection failed or the client is closed.
    Connection(ConnectionError),
    /// The server could not run a query.
    Query(Box<QueryError>),
    /// A query could not be built or failed while it ran.
    Pipeline(PipelineError),
    /// A row's primary key is not a string, number, or boolean.
    InvalidPrimaryKey(InvalidPrimaryKeyValue),
    /// Anything else, with its message.
    Message(String),
}

impl Error {
    /// The error's message.
    pub fn message(&self) -> String {
        match self {
            Error::Application(e) => e.message.clone(),
            Error::Connection(e) => e.message().to_owned(),
            Error::Query(e) => e.to_string(),
            Error::Pipeline(e) => e.to_string(),
            Error::InvalidPrimaryKey(e) => e.to_string(),
            Error::Message(m) => m.clone(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Connection(e) => e.fmt(f),
            e => f.write_str(&e.message()),
        }
    }
}

impl std::error::Error for Error {}

impl From<ApplicationError> for Error {
    fn from(e: ApplicationError) -> Self {
        Error::Application(e)
    }
}

impl From<ConnectionError> for Error {
    fn from(e: ConnectionError) -> Self {
        Error::Connection(e)
    }
}

impl From<QueryError> for Error {
    fn from(e: QueryError) -> Self {
        Error::Query(Box::new(e))
    }
}

impl From<PipelineError> for Error {
    fn from(e: PipelineError) -> Self {
        Error::Pipeline(e)
    }
}

impl From<InvalidPrimaryKeyValue> for Error {
    fn from(e: InvalidPrimaryKeyValue) -> Self {
        Error::InvalidPrimaryKey(e)
    }
}

impl From<String> for Error {
    fn from(m: String) -> Self {
        Error::Message(m)
    }
}

impl From<&str> for Error {
    fn from(m: &str) -> Self {
        Error::Message(m.to_owned())
    }
}
