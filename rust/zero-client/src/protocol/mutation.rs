//! Mutations, pushes, and their results (`packages/zero-protocol/src/mutation.ts`,
//! `push.ts`, and `mutations-patch.ts`).

use super::error::enum_value;
use super::*;
use crate::ast::string_enum;

/// The name of every CRUD mutation (`CRUD_MUTATION_NAME`).
pub const CRUD_MUTATION_NAME: &str = "_zero_crud";

string_enum!(CrudOpKind { Insert = "insert", Upsert = "upsert", Update = "update", Delete = "delete" });

/// One operation of a CRUD mutation (`crudOpSchema`). An update's `value` is
/// a partial row with at least the primary key columns; a delete's holds the
/// primary key columns, each a string, number, or boolean.
#[derive(Clone, Debug, PartialEq)]
pub struct CrudOp {
    pub op: CrudOpKind,
    pub table_name: String,
    pub primary_key: Vec<String>,
    pub value: RowRecord,
}

impl JsonCodable for CrudOp {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "CRUD op")?;
        let name = r.string("op")?;
        let op = CrudOpKind::parse(&name)
            .ok_or_else(|| DecodeError::new(format!("Invalid CRUD op \"{name}\"")).at("op"))?;
        Ok(CrudOp {
            op,
            table_name: r.string("tableName")?,
            primary_key: r.required("primaryKey", decode::compound_key)?,
            value: if op == CrudOpKind::Delete {
                r.required("value", primary_key_value_record)?
            } else {
                r.required("value", decode::object)?
            },
        })
    }

    fn to_json(&self) -> Value {
        Builder::new()
            .set("op", self.op.as_str())
            .set("tableName", self.table_name.clone())
            .set("primaryKey", decode::strings_json(&self.primary_key))
            .set("value", self.value.clone())
            .build()
    }
}

/// A mutation as pushed to zero-cache (`mutationSchema`).
#[derive(Clone, Debug, PartialEq)]
pub enum Mutation {
    /// The legacy CRUD mutators (`crudMutationSchema`). Its name is always
    /// [`CRUD_MUTATION_NAME`] and its args are `[{ops}]`.
    Crud {
        id: f64,
        client_id: String,
        ops: Vec<CrudOp>,
        timestamp: f64,
    },
    /// A custom mutator (`customMutationSchema`).
    Custom {
        id: f64,
        client_id: String,
        name: String,
        args: Vec<Value>,
        timestamp: f64,
    },
}

impl JsonCodable for Mutation {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "mutation")?;
        match r.required("type", |v| enum_value!(MutationType, v))? {
            MutationType::Crud => {
                r.literal("name", CRUD_MUTATION_NAME)?;
                let ops = r.required("args", |v| {
                    let args = decode::array_value(v)?;
                    if args.len() != 1 {
                        return Err(DecodeError::new(format!(
                            "Expected array of length 1, got {}",
                            args.len()
                        )));
                    }
                    Reader::new(&args[0], "CRUD arg")
                        .and_then(|arg| {
                            arg.required("ops", |v| decode::array(v, CrudOp::from_json))
                        })
                        .map_err(|e| e.at("0"))
                })?;
                Ok(Mutation::Crud {
                    id: r.number("id")?,
                    client_id: r.string("clientID")?,
                    ops,
                    timestamp: r.number("timestamp")?,
                })
            }
            MutationType::Custom => Ok(Mutation::Custom {
                id: r.number("id")?,
                client_id: r.string("clientID")?,
                name: r.string("name")?,
                args: r.required("args", |v| Ok(decode::array_value(v)?.clone()))?,
                timestamp: r.number("timestamp")?,
            }),
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Mutation::Crud {
                id,
                client_id,
                ops,
                timestamp,
            } => Builder::new()
                .set("type", "crud")
                .set("id", *id)
                .set("clientID", client_id.clone())
                .set("name", CRUD_MUTATION_NAME)
                .set(
                    "args",
                    Value::Array(vec![Builder::new().set("ops", array_json(ops)).build()]),
                )
                .set("timestamp", *timestamp),
            Mutation::Custom {
                id,
                client_id,
                name,
                args,
                timestamp,
            } => Builder::new()
                .set("type", "custom")
                .set("id", *id)
                .set("clientID", client_id.clone())
                .set("name", name.clone())
                .set("args", args.clone())
                .set("timestamp", *timestamp),
        }
        .build()
    }
}

/// A failed mutation's result (`mutationErrorSchema`), kept as sent: see
/// [`MutationResult`] for which results are errors.
#[derive(Clone, Debug, PartialEq)]
pub struct MutationError {
    /// `"app"` for an error the mutator threw; `"oooMutation"` or
    /// `"alreadyProcessed"` for one from zero.
    pub error: Value,
    pub message: Option<Value>,
    pub details: Option<Value>,
}

/// The result of running a mutation on the API server
/// (`mutationResultSchema`), read as the JS client reads it: in
/// `passthrough` mode, telling errors apart with `'error' in result`. The
/// union's OK branch, `{data?}`, accepts every object, so a result with
/// `data` is OK even if it has an `error`, and any other result with an
/// `error`, whatever its value, is an error.
#[derive(Clone, Debug, PartialEq)]
pub enum MutationResult {
    Ok { data: Option<Value> },
    Error(MutationError),
}

impl JsonCodable for MutationResult {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "mutation result")?;
        Ok(if let Some(data) = r.get("data") {
            MutationResult::Ok {
                data: Some(data.clone()),
            }
        } else if let Some(error) = r.get("error") {
            MutationResult::Error(MutationError {
                error: error.clone(),
                message: r.get("message").cloned(),
                details: r.get("details").cloned(),
            })
        } else {
            MutationResult::Ok { data: None }
        })
    }

    fn to_json(&self) -> Value {
        match self {
            MutationResult::Ok { data } => Builder::new().opt("data", data.clone()),
            MutationResult::Error(e) => Builder::new()
                .set("error", e.error.clone())
                .opt("message", e.message.clone())
                .opt("details", e.details.clone()),
        }
        .build()
    }
}

/// `mutationResponseSchema`.
#[derive(Clone, Debug, PartialEq)]
pub struct MutationResponse {
    pub id: MutationId,
    pub result: MutationResult,
}

impl JsonCodable for MutationResponse {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "mutation response")?;
        Ok(MutationResponse {
            id: r.required("id", MutationId::from_json)?,
            result: r.required("result", MutationResult::from_json)?,
        })
    }

    fn to_json(&self) -> Value {
        Builder::new()
            .set("id", self.id.to_json())
            .set("result", self.result.to_json())
            .build()
    }
}

/// A change to the mutation results zero-cache tracks for a client
/// (`mutations-patch.ts`).
#[derive(Clone, Debug, PartialEq)]
pub enum MutationPatchOp {
    Put(Box<MutationResponse>),
    Del(MutationId),
}

impl JsonCodable for MutationPatchOp {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "mutations patch op")?;
        match r.string("op")?.as_str() {
            "put" => Ok(MutationPatchOp::Put(Box::new(
                r.required("mutation", MutationResponse::from_json)?,
            ))),
            "del" => Ok(MutationPatchOp::Del(
                r.required("id", MutationId::from_json)?,
            )),
            op => Err(DecodeError::new(format!("Invalid mutations patch op \"{op}\"")).at("op")),
        }
    }

    fn to_json(&self) -> Value {
        match self {
            MutationPatchOp::Put(m) => Builder::new().set("op", "put").set("mutation", m.to_json()),
            MutationPatchOp::Del(id) => Builder::new().set("op", "del").set("id", id.to_json()),
        }
        .build()
    }
}

/// The body of a `push` message (`pushBodySchema`).
#[derive(Clone, Debug, PartialEq)]
pub struct PushBody {
    pub client_group_id: String,
    pub mutations: Vec<Mutation>,
    pub push_version: f64,
    /// The schema version of CRUD mutations.
    pub schema_version: Option<f64>,
    pub timestamp: f64,
    pub request_id: String,
    /// Deprecated: auth is set by `initConnection` and `updateAuth`.
    pub auth: Option<String>,
    pub traceparent: Option<String>,
}

impl JsonCodable for PushBody {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "push body")?;
        Ok(PushBody {
            client_group_id: r.string("clientGroupID")?,
            mutations: r.required("mutations", |v| decode::array(v, Mutation::from_json))?,
            push_version: r.number("pushVersion")?,
            schema_version: r.optional_number("schemaVersion")?,
            timestamp: r.number("timestamp")?,
            request_id: r.string("requestID")?,
            auth: r.optional_string("auth")?,
            traceparent: r.optional_string("traceparent")?,
        })
    }

    fn to_json(&self) -> Value {
        Builder::new()
            .set("clientGroupID", self.client_group_id.clone())
            .set("mutations", array_json(&self.mutations))
            .set("pushVersion", self.push_version)
            .opt("schemaVersion", self.schema_version)
            .set("timestamp", self.timestamp)
            .set("requestID", self.request_id.clone())
            .opt("auth", self.auth.clone())
            .opt("traceparent", self.traceparent.clone())
            .build()
    }
}

/// The deprecated error responses to a push (`pushErrorSchema`). zero-cache
/// now reports push failures with `error` messages.
#[derive(Clone, Debug, PartialEq)]
pub enum PushError {
    UnsupportedPushVersion {
        mutation_ids: Option<Vec<MutationId>>,
    },
    UnsupportedSchemaVersion {
        mutation_ids: Option<Vec<MutationId>>,
    },
    Http {
        status: f64,
        details: String,
        mutation_ids: Option<Vec<MutationId>>,
    },
    ZeroPusher {
        details: String,
        mutation_ids: Option<Vec<MutationId>>,
    },
}

impl PushError {
    pub fn mutation_ids(&self) -> Option<&[MutationId]> {
        match self {
            PushError::UnsupportedPushVersion { mutation_ids }
            | PushError::UnsupportedSchemaVersion { mutation_ids }
            | PushError::Http { mutation_ids, .. }
            | PushError::ZeroPusher { mutation_ids, .. } => mutation_ids.as_deref(),
        }
    }
}

impl JsonCodable for PushError {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "push error")?;
        let error = r.string("error")?;
        let ids = || r.optional("mutationIDs", |v| decode::array(v, MutationId::from_json));
        match error.as_str() {
            "unsupportedPushVersion" => Ok(PushError::UnsupportedPushVersion {
                mutation_ids: ids()?,
            }),
            "unsupportedSchemaVersion" => Ok(PushError::UnsupportedSchemaVersion {
                mutation_ids: ids()?,
            }),
            "http" => Ok(PushError::Http {
                status: r.number("status")?,
                details: r.string("details")?,
                mutation_ids: ids()?,
            }),
            "zeroPusher" => Ok(PushError::ZeroPusher {
                details: r.string("details")?,
                mutation_ids: ids()?,
            }),
            _ => Err(DecodeError::new(format!("Invalid push error \"{error}\"")).at("error")),
        }
    }

    fn to_json(&self) -> Value {
        let b = match self {
            PushError::UnsupportedPushVersion { .. } => {
                Builder::new().set("error", "unsupportedPushVersion")
            }
            PushError::UnsupportedSchemaVersion { .. } => {
                Builder::new().set("error", "unsupportedSchemaVersion")
            }
            PushError::Http {
                status, details, ..
            } => Builder::new()
                .set("error", "http")
                .set("status", *status)
                .set("details", details.clone()),
            PushError::ZeroPusher { details, .. } => Builder::new()
                .set("error", "zeroPusher")
                .set("details", details.clone()),
        };
        b.opt("mutationIDs", self.mutation_ids().map(array_json))
            .build()
    }
}

/// The body of a `pushResponse` message (`pushResponseBodySchema`). valita
/// tries the OK variant first, and it has no discriminator: a body with a
/// valid `mutations` array is OK even if it also has an `error` key.
#[derive(Clone, Debug, PartialEq)]
pub enum PushResponseBody {
    Ok { mutations: Vec<MutationResponse> },
    Error(PushError),
}

impl JsonCodable for PushResponseBody {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "pushResponse body")?;
        match r.required("mutations", |v| {
            decode::array(v, MutationResponse::from_json)
        }) {
            Ok(mutations) => Ok(PushResponseBody::Ok { mutations }),
            Err(_) => Ok(PushResponseBody::Error(PushError::from_json(v)?)),
        }
    }

    fn to_json(&self) -> Value {
        match self {
            PushResponseBody::Ok { mutations } => Builder::new()
                .set("mutations", array_json(mutations))
                .build(),
            PushResponseBody::Error(e) => e.to_json(),
        }
    }
}
