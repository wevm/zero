//! Decoding [`Value`]s into typed protocol values, following valita's rules:
//! optional fields may be absent but not `null` (unless declared nullable),
//! and unknown keys are ignored (`passthrough` mode).

use crate::json::{Object, Value};
use std::fmt;

/// Why a value didn't decode. The path points at the offending value, like
/// valita's error paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError {
    pub path: Vec<String>,
    pub message: String,
    /// What valita writes after the path, such as ` Got "x"`.
    pub got: String,
}

impl DecodeError {
    pub fn new(message: impl Into<String>) -> Self {
        DecodeError {
            path: Vec::new(),
            message: message.into(),
            got: String::new(),
        }
    }

    pub fn at_path(mut self, path: &[&str]) -> Self {
        self.path = path.iter().map(|s| (*s).to_owned()).collect();
        self
    }

    /// The error, one key further out.
    pub fn at(mut self, key: impl Into<String>) -> Self {
        self.path.insert(0, key.into());
        self
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(f, "{}{}", self.message, self.got)
        } else {
            write!(f, "{} at {}{}", self.message, self.path.join("."), self.got)
        }
    }
}

impl std::error::Error for DecodeError {}

pub type Result<T, E = DecodeError> = std::result::Result<T, E>;

/// A value with a JSON form following the `zero-protocol` schema.
pub trait JsonCodable: Sized {
    fn from_json(v: &Value) -> Result<Self>;
    fn to_json(&self) -> Value;
}

/// Reads the fields of an object.
#[derive(Clone, Copy)]
pub struct Reader<'a>(pub &'a Object);

impl<'a> Reader<'a> {
    pub fn new(v: &'a Value, what: &str) -> Result<Self> {
        match v {
            Value::Object(o) => Ok(Reader(o)),
            v => Err(DecodeError::new(format!(
                "Expected {what}, got {}",
                v.type_name()
            ))),
        }
    }

    pub fn get(&self, key: &str) -> Option<&'a Value> {
        self.0.get(key)
    }

    pub fn required<T>(&self, key: &str, decode: impl FnOnce(&'a Value) -> Result<T>) -> Result<T> {
        let v = self
            .0
            .get(key)
            .ok_or_else(|| DecodeError::new("Missing property").at(key))?;
        decode(v).map_err(|e| e.at(key))
    }

    pub fn optional<T>(
        &self,
        key: &str,
        decode: impl FnOnce(&'a Value) -> Result<T>,
    ) -> Result<Option<T>> {
        match self.0.get(key) {
            None => Ok(None),
            Some(v) => decode(v).map(Some).map_err(|e| e.at(key)),
        }
    }

    pub fn string(&self, key: &str) -> Result<String> {
        self.required(key, string)
    }

    pub fn optional_string(&self, key: &str) -> Result<Option<String>> {
        self.optional(key, string)
    }

    pub fn number(&self, key: &str) -> Result<f64> {
        self.required(key, number)
    }

    pub fn optional_number(&self, key: &str) -> Result<Option<f64>> {
        self.optional(key, number)
    }

    pub fn bool(&self, key: &str) -> Result<bool> {
        self.required(key, bool)
    }

    pub fn optional_bool(&self, key: &str) -> Result<Option<bool>> {
        self.optional(key, bool)
    }

    /// `v.literal(expected)`.
    pub fn literal(&self, key: &str, expected: &str) -> Result<()> {
        let s = self.string(key)?;
        if s != expected {
            return Err(
                DecodeError::new(format!("Expected literal \"{expected}\", got \"{s}\"")).at(key),
            );
        }
        Ok(())
    }
}

pub fn string(v: &Value) -> Result<String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        v => Err(DecodeError::new(format!(
            "Expected string, got {}",
            v.type_name()
        ))),
    }
}

pub fn number(v: &Value) -> Result<f64> {
    match v {
        Value::Number(n) => Ok(*n),
        v => Err(DecodeError::new(format!(
            "Expected number, got {}",
            v.type_name()
        ))),
    }
}

pub fn bool(v: &Value) -> Result<bool> {
    match v {
        Value::Bool(b) => Ok(*b),
        v => Err(DecodeError::new(format!(
            "Expected boolean, got {}",
            v.type_name()
        ))),
    }
}

pub fn object(v: &Value) -> Result<Object> {
    match v {
        Value::Object(o) => Ok(o.clone()),
        v => Err(DecodeError::new(format!(
            "Expected object, got {}",
            v.type_name()
        ))),
    }
}

pub fn array_value(v: &Value) -> Result<&Vec<Value>> {
    match v {
        Value::Array(a) => Ok(a),
        v => Err(DecodeError::new(format!(
            "Expected array, got {}",
            v.type_name()
        ))),
    }
}

/// `v.array(element)`.
pub fn array<T>(v: &Value, mut element: impl FnMut(&Value) -> Result<T>) -> Result<Vec<T>> {
    array_value(v)?
        .iter()
        .enumerate()
        .map(|(i, e)| element(e).map_err(|err| err.at(i.to_string())))
        .collect()
}

pub fn strings(v: &Value) -> Result<Vec<String>> {
    array(v, string)
}

/// A non-empty array of strings (`CompoundKey`, `PrimaryKey`).
pub fn compound_key(v: &Value) -> Result<Vec<String>> {
    let k = strings(v)?;
    if k.is_empty() {
        return Err(DecodeError::new("Expected non-empty array"));
    }
    Ok(k)
}

/// `v.record(element)`, keeping the object's order.
pub fn record<T>(
    v: &Value,
    mut element: impl FnMut(&Value) -> Result<T>,
) -> Result<Vec<(String, T)>> {
    let Value::Object(o) = v else {
        return Err(DecodeError::new(format!(
            "Expected object, got {}",
            v.type_name()
        )));
    };
    o.iter()
        .map(|(k, e)| Ok((k.clone(), element(e).map_err(|err| err.at(k.clone()))?)))
        .collect()
}

/// A string, number, or boolean (`primaryKeyValueSchema`).
pub fn primary_key_value(v: &Value) -> Result<Value> {
    match v {
        Value::String(_) | Value::Number(_) | Value::Bool(_) => Ok(v.clone()),
        v => Err(DecodeError::new(format!(
            "Expected string, number, or boolean, got {}",
            v.type_name()
        ))),
    }
}

/// A tuple message `[tag, body]`.
pub fn tagged(v: &Value) -> Result<(&str, &Value)> {
    match v {
        Value::Array(a) if a.len() == 2 => match &a[0] {
            Value::String(tag) => Ok((tag, &a[1])),
            _ => Err(DecodeError::new("Expected [tag, body] tuple")),
        },
        _ => Err(DecodeError::new("Expected [tag, body] tuple")),
    }
}

/// Builds a JSON object, leaving out `None` fields.
#[derive(Default)]
pub struct Builder(pub Object);

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.0.insert(key, value.into());
        self
    }

    pub fn opt(mut self, key: &str, value: Option<impl Into<Value>>) -> Self {
        if let Some(v) = value {
            self.0.insert(key, v.into());
        }
        self
    }

    pub fn build(self) -> Value {
        Value::Object(self.0)
    }
}

/// Strings as a JSON array.
pub fn strings_json(v: &[String]) -> Value {
    Value::Array(v.iter().map(|s| Value::String(s.clone())).collect())
}
