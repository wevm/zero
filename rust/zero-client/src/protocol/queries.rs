//! Query patches (`queries-patch.ts`) and the small message bodies:
//! `changeDesiredQueries`, `pull`, `deleteClients`, `updateAuth`, and
//! `transformError`.

use super::*;
use crate::ast::{Ast, strict};

/// A change to a set of queries as zero-cache sends it (`patchOpSchema`).
#[derive(Clone, Debug, PartialEq)]
pub enum QueriesPatchOp {
    Put { hash: String, ttl: Option<f64> },
    Del { hash: String },
    Clear,
}

impl JsonCodable for QueriesPatchOp {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "queries patch op")?;
        match r.string("op")?.as_str() {
            "put" => Ok(QueriesPatchOp::Put {
                hash: r.string("hash")?,
                ttl: r.optional_number("ttl")?,
            }),
            "del" => Ok(QueriesPatchOp::Del {
                hash: r.string("hash")?,
            }),
            "clear" => Ok(QueriesPatchOp::Clear),
            op => Err(DecodeError::new(format!("Invalid queries patch op \"{op}\"")).at("op")),
        }
    }

    fn to_json(&self) -> Value {
        match self {
            QueriesPatchOp::Put { hash, ttl } => Builder::new()
                .set("op", "put")
                .set("hash", hash.clone())
                .opt("ttl", *ttl)
                .build(),
            QueriesPatchOp::Del { hash } => Builder::new()
                .set("op", "del")
                .set("hash", hash.clone())
                .build(),
            QueriesPatchOp::Clear => Builder::new().set("op", "clear").build(),
        }
    }
}

pub type QueriesPatch = Vec<QueriesPatchOp>;

pub(crate) fn queries_patch(v: &Value) -> Result<QueriesPatch> {
    decode::array(v, QueriesPatchOp::from_json)
}

/// A query a client wants synced (`upPutOpSchema`). A client query carries
/// its `ast`; a custom query its `name` and `args`.
#[derive(Clone, Debug, PartialEq)]
pub struct UpQueriesPut {
    pub hash: String,
    pub ttl: Option<f64>,
    pub ast: Option<Ast>,
    pub name: Option<String>,
    pub args: Option<Vec<Value>>,
}

/// A change to a client's desired queries, as the client sends it
/// (`upPatchOpSchema`).
#[derive(Clone, Debug, PartialEq)]
pub enum UpQueriesPatchOp {
    Put(Box<UpQueriesPut>),
    Del { hash: String },
    Clear,
}

impl JsonCodable for UpQueriesPatchOp {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "queries patch op")?;
        match r.string("op")?.as_str() {
            "put" => Ok(UpQueriesPatchOp::Put(Box::new(UpQueriesPut {
                hash: r.string("hash")?,
                ttl: r.optional_number("ttl")?,
                ast: r.optional("ast", strict::decode)?,
                name: r.optional_string("name")?,
                args: r.optional("args", |v| Ok(decode::array_value(v)?.clone()))?,
            }))),
            "del" => Ok(UpQueriesPatchOp::Del {
                hash: r.string("hash")?,
            }),
            "clear" => Ok(UpQueriesPatchOp::Clear),
            op => Err(DecodeError::new(format!("Invalid queries patch op \"{op}\"")).at("op")),
        }
    }

    fn to_json(&self) -> Value {
        match self {
            UpQueriesPatchOp::Put(p) => Builder::new()
                .set("op", "put")
                .set("hash", p.hash.clone())
                .opt("ttl", p.ttl)
                .opt("ast", p.ast.as_ref().map(Ast::to_json))
                .opt("name", p.name.clone())
                .opt("args", p.args.clone())
                .build(),
            UpQueriesPatchOp::Del { hash } => Builder::new()
                .set("op", "del")
                .set("hash", hash.clone())
                .build(),
            UpQueriesPatchOp::Clear => Builder::new().set("op", "clear").build(),
        }
    }
}

pub type UpQueriesPatch = Vec<UpQueriesPatchOp>;

pub(crate) fn up_queries_patch(v: &Value) -> Result<UpQueriesPatch> {
    decode::array(v, UpQueriesPatchOp::from_json)
}

/// `changeDesiredQueries` (`change-desired-queries.ts`).
#[derive(Clone, Debug, PartialEq)]
pub struct ChangeDesiredQueriesBody {
    pub desired_queries_patch: UpQueriesPatch,
    /// A W3C traceparent header for distributed tracing.
    pub traceparent: Option<String>,
}

impl JsonCodable for ChangeDesiredQueriesBody {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "changeDesiredQueries body")?;
        Ok(ChangeDesiredQueriesBody {
            desired_queries_patch: r.required("desiredQueriesPatch", up_queries_patch)?,
            traceparent: r.optional_string("traceparent")?,
        })
    }

    fn to_json(&self) -> Value {
        Builder::new()
            .set(
                "desiredQueriesPatch",
                array_json(&self.desired_queries_patch),
            )
            .opt("traceparent", self.traceparent.clone())
            .build()
    }
}

/// A `pull` request (`pullRequestBodySchema`).
#[derive(Clone, Debug, PartialEq)]
pub struct PullRequestBody {
    pub client_group_id: String,
    pub cookie: Option<Version>,
    pub request_id: String,
}

impl JsonCodable for PullRequestBody {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "pull request body")?;
        Ok(PullRequestBody {
            client_group_id: r.string("clientGroupID")?,
            cookie: r.required("cookie", nullable_version)?,
            request_id: r.string("requestID")?,
        })
    }

    fn to_json(&self) -> Value {
        Builder::new()
            .set("clientGroupID", self.client_group_id.clone())
            .set("cookie", nullable_version_json(&self.cookie))
            .set("requestID", self.request_id.clone())
            .build()
    }
}

/// A `pull` response (`pullResponseBodySchema`). Pull only recovers
/// mutations, so it carries no patch.
#[derive(Clone, Debug, PartialEq)]
pub struct PullResponseBody {
    pub cookie: Version,
    /// The `requestID` of the pull request this answers.
    pub request_id: String,
    pub last_mutation_id_changes: Vec<(String, f64)>,
}

impl JsonCodable for PullResponseBody {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "pull response body")?;
        Ok(PullResponseBody {
            cookie: r.string("cookie")?,
            request_id: r.string("requestID")?,
            last_mutation_id_changes: r.required("lastMutationIDChanges", number_record)?,
        })
    }

    fn to_json(&self) -> Value {
        Builder::new()
            .set("cookie", self.cookie.clone())
            .set("requestID", self.request_id.clone())
            .set(
                "lastMutationIDChanges",
                number_record_json(&self.last_mutation_id_changes),
            )
            .build()
    }
}

/// `deleteClients`, in either direction (`delete-clients.ts`).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct DeleteClientsBody {
    pub client_ids: Option<Vec<String>>,
    pub client_group_ids: Option<Vec<String>>,
}

impl JsonCodable for DeleteClientsBody {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "deleteClients body")?;
        Ok(DeleteClientsBody {
            client_ids: r.optional("clientIDs", decode::strings)?,
            client_group_ids: r.optional("clientGroupIDs", decode::strings)?,
        })
    }

    fn to_json(&self) -> Value {
        Builder::new()
            .opt(
                "clientIDs",
                self.client_ids.as_deref().map(decode::strings_json),
            )
            .opt(
                "clientGroupIDs",
                self.client_group_ids.as_deref().map(decode::strings_json),
            )
            .build()
    }
}

/// `updateAuth` (`update-auth.ts`).
#[derive(Clone, Debug, PartialEq)]
pub struct UpdateAuthBody {
    pub auth: String,
}

impl JsonCodable for UpdateAuthBody {
    fn from_json(v: &Value) -> Result<Self> {
        Ok(UpdateAuthBody {
            auth: Reader::new(v, "updateAuth body")?.string("auth")?,
        })
    }

    fn to_json(&self) -> Value {
        Builder::new().set("auth", self.auth.clone()).build()
    }
}

/// A custom query the API server failed to transform (`erroredQuerySchema`
/// in `custom-queries.ts`).
#[derive(Clone, Debug, PartialEq)]
pub enum ErroredQuery {
    /// The query function threw. `message` is optional for backwards
    /// compatibility.
    App {
        id: String,
        name: String,
        message: Option<String>,
        details: Option<Value>,
    },
    /// The query's arguments failed to parse.
    Parse {
        id: String,
        name: String,
        message: String,
        details: Option<Value>,
    },
}

impl JsonCodable for ErroredQuery {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "errored query")?;
        let details = r.get("details").cloned();
        match r.string("error")?.as_str() {
            "app" => Ok(ErroredQuery::App {
                id: r.string("id")?,
                name: r.string("name")?,
                message: r.optional_string("message")?,
                details,
            }),
            "parse" => Ok(ErroredQuery::Parse {
                id: r.string("id")?,
                name: r.string("name")?,
                message: r.string("message")?,
                details,
            }),
            e => Err(DecodeError::new(format!("Invalid query error \"{e}\"")).at("error")),
        }
    }

    fn to_json(&self) -> Value {
        let (error, id, name, message, details) = match self {
            ErroredQuery::App {
                id,
                name,
                message,
                details,
            } => ("app", id, name, message.clone(), details),
            ErroredQuery::Parse {
                id,
                name,
                message,
                details,
            } => ("parse", id, name, Some(message.clone()), details),
        };
        Builder::new()
            .set("error", error)
            .set("id", id.clone())
            .set("name", name.clone())
            .opt("message", message)
            .opt("details", details.clone())
            .build()
    }
}
