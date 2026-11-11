//! The body of an `error` message (`errorBodySchema` in
//! `packages/zero-protocol/src/error.ts`) and its enums.

use super::*;
use crate::ast::string_enum;

string_enum!(
    /// The kind of a protocol error (`error-kind-enum.ts`), in the order of
    /// `errorKindSchema`, which valita's messages list.
    ErrorKind {
        AuthInvalidated = "AuthInvalidated",
        ClientNotFound = "ClientNotFound",
        InvalidConnectionRequest = "InvalidConnectionRequest",
        InvalidConnectionRequestBaseCookie = "InvalidConnectionRequestBaseCookie",
        InvalidConnectionRequestLastMutationID = "InvalidConnectionRequestLastMutationID",
        InvalidConnectionRequestClientDeleted = "InvalidConnectionRequestClientDeleted",
        InvalidMessage = "InvalidMessage",
        InvalidPush = "InvalidPush",
        MutationRateLimited = "MutationRateLimited",
        MutationFailed = "MutationFailed",
        Unauthorized = "Unauthorized",
        VersionNotSupported = "VersionNotSupported",
        SchemaVersionNotSupported = "SchemaVersionNotSupported",
        Internal = "Internal",
        Rebalance = "Rebalance",
        Rehome = "Rehome",
        ServerOverloaded = "ServerOverloaded",
        PushFailed = "PushFailed",
        TransformFailed = "TransformFailed",
    }
);

impl ErrorKind {
    /// The kinds whose body carries backoff parameters
    /// (`backoffErrorKindSchema`).
    pub fn is_backoff(self) -> bool {
        matches!(
            self,
            ErrorKind::Rebalance | ErrorKind::Rehome | ErrorKind::ServerOverloaded
        )
    }

    /// The kinds of a basic error body (`basicErrorKindSchema`).
    pub fn is_basic(self) -> bool {
        !self.is_backoff() && !matches!(self, ErrorKind::PushFailed | ErrorKind::TransformFailed)
    }
}

string_enum!(
    /// Where an error came from (`error-origin-enum.ts`).
    ErrorOrigin { Client = "client", Server = "server", ZeroCache = "zeroCache" }
);

string_enum!(
    /// Why a push or transform failed (`error-reason-enum.ts`).
    ErrorReason {
        Database = "database",
        Parse = "parse",
        OutOfOrderMutation = "oooMutation",
        UnsupportedPushVersion = "unsupportedPushVersion",
        Internal = "internal",
        Http = "http",
        Timeout = "timeout",
    }
);

string_enum!(
    /// `mutation-type-enum.ts`.
    MutationType { Crud = "crud", Custom = "custom" }
);

/// Decodes one of a [`string_enum!`]'s values with valita's message.
macro_rules! enum_value {
    ($ty:ident, $v:expr) => {{
        let names: Vec<&str> = $ty::ALL.iter().map(|k| k.as_str()).collect();
        literal_union($v, &names).map(|n| $ty::parse(n).expect("listed"))
    }};
}
pub(crate) use enum_value;

/// An origin other than `client`.
fn remote_origin(v: &Value) -> Result<ErrorOrigin> {
    match enum_value!(ErrorOrigin, v)? {
        ErrorOrigin::Client => Err(DecodeError::new("Unexpected origin \"client\"")),
        o => Ok(o),
    }
}

/// Why a push or transform failed: the `origin` and `reason` of
/// `pushFailedBodySchema` and `transformFailedBodySchema`, whose three
/// variants each allow different combinations.
#[derive(Clone, Debug, PartialEq)]
pub enum FailureCause {
    /// `origin: 'server'`: the API server reported the failure.
    Server { reason: ErrorReason },
    /// `origin: 'zeroCache', reason: 'http'`: the API server answered with a
    /// non-OK status.
    Http {
        status: f64,
        body_preview: Option<String>,
    },
    /// `origin: 'zeroCache'` with reason `timeout`, `parse`, or `internal`.
    ZeroCache { reason: ErrorReason },
}

impl FailureCause {
    pub fn origin(&self) -> ErrorOrigin {
        match self {
            FailureCause::Server { .. } => ErrorOrigin::Server,
            _ => ErrorOrigin::ZeroCache,
        }
    }

    pub fn reason(&self) -> ErrorReason {
        match self {
            FailureCause::Server { reason } | FailureCause::ZeroCache { reason } => *reason,
            FailureCause::Http { .. } => ErrorReason::Http,
        }
    }

    fn decode(r: &Reader<'_>, server_reasons: &[ErrorReason]) -> Result<Self> {
        let one_of = |allowed: &[ErrorReason]| {
            r.required("reason", |v| {
                let reason = enum_value!(ErrorReason, v)?;
                if allowed.contains(&reason) {
                    Ok(reason)
                } else {
                    Err(DecodeError::new(format!("Unexpected value {v}")))
                }
            })
        };
        // valita tries the variants in order; at most one accepts.
        if r.required("origin", remote_origin)? == ErrorOrigin::Server {
            return Ok(FailureCause::Server {
                reason: one_of(server_reasons)?,
            });
        }
        if r.required("reason", |v| enum_value!(ErrorReason, v))? == ErrorReason::Http {
            return Ok(FailureCause::Http {
                status: r.number("status")?,
                body_preview: r.optional_string("bodyPreview")?,
            });
        }
        Ok(FailureCause::ZeroCache {
            reason: one_of(&[
                ErrorReason::Timeout,
                ErrorReason::Parse,
                ErrorReason::Internal,
            ])?,
        })
    }

    fn write(&self, b: Builder) -> Builder {
        let b = b
            .set("origin", self.origin().as_str())
            .set("reason", self.reason().as_str());
        match self {
            FailureCause::Http {
                status,
                body_preview,
            } => b
                .set("status", *status)
                .opt("bodyPreview", body_preview.clone()),
            _ => b,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ErrorBody {
    /// `basicErrorBodySchema`. `origin` is `server` or `zeroCache` when set.
    Basic {
        kind: ErrorKind,
        message: String,
        origin: Option<ErrorOrigin>,
    },
    /// `backoffBodySchema`: zero-cache asks the client to reconnect later.
    Backoff {
        kind: ErrorKind,
        message: String,
        min_backoff_ms: Option<f64>,
        max_backoff_ms: Option<f64>,
        /// Query parameters to send in the next reconnect only.
        reconnect_params: Option<Vec<(String, String)>>,
        origin: Option<ErrorOrigin>,
    },
    /// `pushFailedBodySchema`: `mutation_ids` may be a subset of the pushed
    /// mutations.
    PushFailed {
        message: String,
        mutation_ids: Vec<MutationId>,
        details: Option<Value>,
        cause: FailureCause,
    },
    /// `transformFailedBodySchema`.
    TransformFailed {
        message: String,
        query_ids: Vec<String>,
        details: Option<Value>,
        cause: FailureCause,
    },
}

const PUSH_SERVER_REASONS: &[ErrorReason] = &[
    ErrorReason::Database,
    ErrorReason::Parse,
    ErrorReason::OutOfOrderMutation,
    ErrorReason::UnsupportedPushVersion,
    ErrorReason::Internal,
];

const TRANSFORM_SERVER_REASONS: &[ErrorReason] = &[
    ErrorReason::Database,
    ErrorReason::Parse,
    ErrorReason::Internal,
];

impl ErrorBody {
    pub fn kind(&self) -> ErrorKind {
        match self {
            ErrorBody::Basic { kind, .. } | ErrorBody::Backoff { kind, .. } => *kind,
            ErrorBody::PushFailed { .. } => ErrorKind::PushFailed,
            ErrorBody::TransformFailed { .. } => ErrorKind::TransformFailed,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            ErrorBody::Basic { message, .. }
            | ErrorBody::Backoff { message, .. }
            | ErrorBody::PushFailed { message, .. }
            | ErrorBody::TransformFailed { message, .. } => message,
        }
    }

    /// Where the error came from; `None` from servers too old to say.
    pub fn origin(&self) -> Option<ErrorOrigin> {
        match self {
            ErrorBody::Basic { origin, .. } | ErrorBody::Backoff { origin, .. } => *origin,
            ErrorBody::PushFailed { cause, .. } | ErrorBody::TransformFailed { cause, .. } => {
                Some(cause.origin())
            }
        }
    }
}

impl JsonCodable for ErrorBody {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "error body")?;
        let kind = r.required("kind", |v| enum_value!(ErrorKind, v))?;
        if kind.is_basic() {
            return Ok(ErrorBody::Basic {
                kind,
                message: r.string("message")?,
                origin: r.optional("origin", remote_origin)?,
            });
        }
        if kind.is_backoff() {
            return Ok(ErrorBody::Backoff {
                kind,
                message: r.string("message")?,
                min_backoff_ms: r.optional_number("minBackoffMs")?,
                max_backoff_ms: r.optional_number("maxBackoffMs")?,
                reconnect_params: r.optional("reconnectParams", string_record)?,
                origin: r.optional("origin", |v| {
                    literal(v, ErrorOrigin::ZeroCache.as_str()).map(|()| ErrorOrigin::ZeroCache)
                })?,
            });
        }
        if kind == ErrorKind::PushFailed {
            return Ok(ErrorBody::PushFailed {
                message: r.string("message")?,
                mutation_ids: r
                    .required("mutationIDs", |v| decode::array(v, MutationId::from_json))?,
                details: r.get("details").cloned(),
                cause: FailureCause::decode(&r, PUSH_SERVER_REASONS)?,
            });
        }
        Ok(ErrorBody::TransformFailed {
            message: r.string("message")?,
            query_ids: r.required("queryIDs", decode::strings)?,
            details: r.get("details").cloned(),
            cause: FailureCause::decode(&r, TRANSFORM_SERVER_REASONS)?,
        })
    }

    fn to_json(&self) -> Value {
        let b = Builder::new()
            .set("kind", self.kind().as_str())
            .set("message", self.message());
        match self {
            ErrorBody::Basic { origin, .. } => b.opt("origin", origin.map(ErrorOrigin::as_str)),
            ErrorBody::Backoff {
                min_backoff_ms,
                max_backoff_ms,
                reconnect_params,
                origin,
                ..
            } => b
                .opt("minBackoffMs", *min_backoff_ms)
                .opt("maxBackoffMs", *max_backoff_ms)
                .opt(
                    "reconnectParams",
                    reconnect_params.as_deref().map(string_record_json),
                )
                .opt("origin", origin.map(ErrorOrigin::as_str)),
            ErrorBody::PushFailed {
                mutation_ids,
                details,
                cause,
                ..
            } => cause.write(
                b.set("mutationIDs", array_json(mutation_ids))
                    .opt("details", details.clone()),
            ),
            ErrorBody::TransformFailed {
                query_ids,
                details,
                cause,
                ..
            } => cause.write(
                b.set("queryIDs", decode::strings_json(query_ids))
                    .opt("details", details.clone()),
            ),
        }
        .build()
    }
}
