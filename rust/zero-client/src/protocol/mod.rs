//! The zero-protocol wire messages (`packages/zero-protocol/src`), decoded
//! as valita does in `passthrough` mode: unknown keys are accepted and
//! dropped, and optional fields may be absent but not `null` unless they
//! are nullable.

mod client_schema;
mod connect;
mod down;
mod error;
mod inspect;
mod mutation;
mod poke;
mod queries;
mod up;
pub mod valita;

pub use client_schema::*;
pub use connect::*;
pub use down::*;
pub use error::*;
pub use inspect::*;
pub use mutation::*;
pub use poke::*;
pub use queries::*;
pub use up::*;

use crate::decode::{self, Builder, DecodeError, JsonCodable, Reader, Result};
use crate::json::{self, Object, Value};

/// The sync protocol version this client speaks
/// (`PROTOCOL_VERSION` in `packages/zero-protocol/src/protocol-version.ts`).
pub const PROTOCOL_VERSION: u32 = 57;

/// A cookie: the version of the client view a client has. Where the
/// protocol allows a `NullableVersion`, `None` is the state before the first
/// pull.
pub type Version = String;

/// The values of a row's primary key columns, by column. Each is a string,
/// number, or boolean.
pub type PrimaryKeyValueRecord = Object;

/// A row as the protocol carries it (`rowSchema`): a record of JSON values.
pub type RowRecord = Object;

/// `packages/zero-protocol/src/mutation-id.ts`.
#[derive(Clone, Debug, PartialEq)]
pub struct MutationId {
    pub id: f64,
    pub client_id: String,
}

impl JsonCodable for MutationId {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "mutation ID")?;
        Ok(MutationId {
            id: r.number("id")?,
            client_id: r.string("clientID")?,
        })
    }

    fn to_json(&self) -> Value {
        Builder::new()
            .set("id", self.id)
            .set("clientID", self.client_id.clone())
            .build()
    }
}

/// `nullableVersionSchema`: a string or `null`.
pub(crate) fn nullable_version(v: &Value) -> Result<Option<Version>> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        v => Err(DecodeError::new(format!(
            "Expected string or null, got {}",
            v.type_name()
        ))),
    }
}

pub(crate) fn nullable_version_json(v: &Option<Version>) -> Value {
    v.clone().map_or(Value::Null, Value::String)
}

/// `v.literal(expected)` for a string.
pub(crate) fn literal(v: &Value, expected: &str) -> Result<()> {
    match v {
        Value::String(s) if s == expected => Ok(()),
        v => Err(DecodeError::new(format!(
            "Expected literal \"{expected}\", got {v}"
        ))),
    }
}

/// `v.literalUnion(...)`: one of `names`, with valita's message.
pub(crate) fn literal_union<'a>(v: &Value, names: &[&'a str]) -> Result<&'a str> {
    if let Value::String(s) = v
        && let Some(name) = names.iter().find(|n| **n == s.as_str())
    {
        return Ok(name);
    }
    let quoted: Vec<String> = names
        .iter()
        .map(|n| json::stringify(&Value::from(*n)))
        .collect();
    let list = match quoted.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
        _ => quoted.concat(),
    };
    let mut e = DecodeError::new(format!("Expected literal value {list}"));
    e.got = format!(" Got {}", crate::keys::valita_display(Some(v)));
    Err(e)
}

/// `v.record(v.string())`, in the object's order.
pub(crate) fn string_record(v: &Value) -> Result<Vec<(String, String)>> {
    decode::record(v, decode::string)
}

/// `v.record(v.number())`, in the object's order.
pub(crate) fn number_record(v: &Value) -> Result<Vec<(String, f64)>> {
    decode::record(v, decode::number)
}

pub(crate) fn string_record_json(r: &[(String, String)]) -> Value {
    Value::Object(
        r.iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect(),
    )
}

pub(crate) fn number_record_json(r: &[(String, f64)]) -> Value {
    Value::Object(
        r.iter()
            .map(|(k, v)| (k.clone(), Value::Number(*v)))
            .collect(),
    )
}

/// `primaryKeyValueRecordSchema`.
pub(crate) fn primary_key_value_record(v: &Value) -> Result<Object> {
    Ok(decode::record(v, decode::primary_key_value)?
        .into_iter()
        .collect())
}

/// `jsonObjectSchema` (`packages/shared/src/json-schema.ts`). It checks
/// `typeof v === 'object'`, so it accepts arrays as well as objects.
pub(crate) fn json_object(v: &Value) -> Result<Value> {
    match v {
        Value::Object(_) | Value::Array(_) => Ok(v.clone()),
        _ => Err(DecodeError::new("Not a JSON object")),
    }
}

/// The body of a `[tag, body]` tuple: exactly two elements, the first the
/// literal `tag`.
pub(crate) fn body_of<'a>(v: &'a Value, tag: &str) -> Result<&'a Value> {
    let a = decode::array_value(v)?;
    if a.len() != 2 {
        return Err(DecodeError::new(format!(
            "Expected array of length 2, got {}",
            a.len()
        )));
    }
    literal(&a[0], tag).map_err(|e| e.at("0"))?;
    Ok(&a[1])
}

/// A `[tag, body]` message.
pub(crate) fn message(tag: &str, body: Value) -> Value {
    Value::Array(vec![Value::from(tag), body])
}

/// Parses the text of a WebSocket message.
pub fn parse_message(text: &str) -> Result<Value> {
    json::parse(text).map_err(|e| DecodeError::new(e.to_string()))
}

pub(crate) fn array_json<T: JsonCodable>(items: &[T]) -> Value {
    Value::Array(items.iter().map(JsonCodable::to_json).collect())
}
