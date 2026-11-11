//! A small interpreter of valita schemas (`packages/shared/src/valita.ts`),
//! for messages kept as JSON rather than decoded into Rust types.
//!
//! [`Schema::parse`] accepts what valita accepts in `passthrough` mode and
//! returns what it returns in `strip` mode: objects lose the keys their
//! schema doesn't name. A union picks its first option that accepts.

use super::literal;
use crate::decode::{self, DecodeError, Reader, Result};
use crate::json::{Object, Value};

#[derive(Clone)]
pub enum Schema {
    String,
    Number,
    Boolean,
    Null,
    /// `v.unknown()` and `jsonSchema`.
    Any,
    Literal(&'static str),
    Array(Box<Schema>),
    /// `v.tuple([s]).concat(v.array(s))`.
    NonEmptyArray(Box<Schema>),
    Tuple(Vec<Schema>),
    Record(Box<Schema>),
    Object(Vec<(&'static str, Field)>),
    Union(Vec<Schema>),
    Nullable(Box<Schema>),
    /// A schema with a Rust decoder; returns the decoded value's JSON.
    Custom(fn(&Value) -> Result<Value>),
}

#[derive(Clone)]
pub enum Field {
    Required(Schema),
    Optional(Schema),
}

impl Schema {
    pub fn parse(&self, v: &Value) -> Result<Value> {
        match self {
            Schema::String => decode::string(v).map(|_| v.clone()),
            Schema::Number => decode::number(v).map(|_| v.clone()),
            Schema::Boolean => decode::bool(v).map(|_| v.clone()),
            Schema::Null if v.is_null() => Ok(Value::Null),
            Schema::Null => Err(DecodeError::new(format!(
                "Expected null, got {}",
                v.type_name()
            ))),
            Schema::Any => Ok(v.clone()),
            Schema::Literal(expected) => literal(v, expected).map(|()| v.clone()),
            Schema::Array(e) => decode::array(v, |x| e.parse(x)).map(Value::Array),
            Schema::NonEmptyArray(e) => {
                let a = decode::array(v, |x| e.parse(x))?;
                if a.is_empty() {
                    return Err(DecodeError::new("Expected non-empty array"));
                }
                Ok(Value::Array(a))
            }
            Schema::Tuple(elements) => {
                let a = decode::array_value(v)?;
                if a.len() != elements.len() {
                    return Err(DecodeError::new(format!(
                        "Expected array of length {}, got {}",
                        elements.len(),
                        a.len()
                    )));
                }
                a.iter()
                    .zip(elements)
                    .enumerate()
                    .map(|(i, (e, s))| s.parse(e).map_err(|err| err.at(i.to_string())))
                    .collect::<Result<_>>()
                    .map(Value::Array)
            }
            Schema::Record(e) => Ok(Value::Object(
                decode::record(v, |x| e.parse(x))?.into_iter().collect(),
            )),
            Schema::Object(fields) => {
                let r = Reader::new(v, "object")?;
                let mut parsed: Vec<(&str, Value)> = Vec::new();
                for (key, field) in fields {
                    let value = match field {
                        Field::Required(s) => Some(r.required(key, |x| s.parse(x))?),
                        Field::Optional(s) => r.optional(key, |x| s.parse(x))?,
                    };
                    if let Some(value) = value {
                        parsed.push((key, value));
                    }
                }
                // In the input's order, as valita keeps it.
                let mut out = Object::new();
                for key in r.0.keys() {
                    if let Some((_, value)) = parsed.iter().find(|(k, _)| k == key) {
                        out.insert(key.clone(), value.clone());
                    }
                }
                Ok(Value::Object(out))
            }
            Schema::Union(options) => {
                options.iter().find_map(|o| o.parse(v).ok()).ok_or_else(|| {
                    DecodeError::new(format!("No union option matched the {}", v.type_name()))
                })
            }
            Schema::Nullable(_) if v.is_null() => Ok(Value::Null),
            Schema::Nullable(s) => s.parse(v),
            Schema::Custom(decode) => decode(v),
        }
    }
}
