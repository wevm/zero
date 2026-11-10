use indexmap::IndexMap;
use std::fmt;

/// A JSON value, the data model Zero syncs and stores.
///
/// Numbers are `f64`, as they are in JS: integers are exact up to 2^53.
/// Objects are [`Object`]s, which keep their keys in JS property order.
///
/// Rust strings compare by code point, which for valid strings is the same
/// as comparing by UTF-16 code unit, as JS `===` does. A Rust `String` can't
/// hold a lone surrogate, which a JS string can (see `rust/PARITY.md`).
#[derive(Clone, Debug, PartialEq, Default)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(Object),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&Object> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }

    /// The value of `key` when this is an object that has it.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_object().and_then(|o| o.get(key))
    }

    /// The element at `index` when this is an array that long.
    pub fn at(&self, index: usize) -> Option<&Value> {
        self.as_array().and_then(|a| a.get(index))
    }

    /// The JS `typeof`-style name of the value's type, for error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }
}

impl fmt::Display for Value {
    /// The value as `JSON.stringify` writes it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&super::stringify(self))
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Value::Number(n)
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::Number(n as f64)
    }
}

impl From<i32> for Value {
    fn from(n: i32) -> Self {
        Value::Number(n.into())
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.to_owned())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
    }
}

impl From<Vec<Value>> for Value {
    fn from(a: Vec<Value>) -> Self {
        Value::Array(a)
    }
}

impl From<Object> for Value {
    fn from(o: Object) -> Self {
        Value::Object(o)
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map_or(Value::Null, Into::into)
    }
}

/// A JSON object: string keys to [`Value`]s, with keys kept the way a JS
/// object keeps its properties.
///
/// Keys are in JS property order: integer-like keys (array indices, such as
/// `"0"` and `"10"`, but not `"01"`) first in ascending numeric order, then
/// the others in the order they were first set. This is the order
/// `Object.keys` and `JSON.stringify` use.
///
/// Setting a key that is already there replaces its value and keeps its
/// place, as assigning to an existing property does in JS. Two objects are
/// equal when they have the same keys with equal values, in any order.
#[derive(Clone, Default, PartialEq)]
pub struct Object {
    entries: IndexMap<String, Value>,
    /// How many of the keys, at the front, are integer-like.
    integer_keys: usize,
}

impl Object {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Object {
            entries: IndexMap::with_capacity(capacity),
            integer_keys: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries.get(key)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.entries.get_mut(key)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// Sets `key` to `value`, returning the value it replaced. A new key goes
    /// after the others, or among the integer-like keys in numeric order.
    pub fn insert(&mut self, key: impl Into<String>, value: Value) -> Option<Value> {
        let key = key.into();
        if let Some(slot) = self.entries.get_mut(&key) {
            return Some(std::mem::replace(slot, value));
        }
        if let Some(n) = array_index(&key) {
            let at = self
                .entries
                .keys()
                .take(self.integer_keys)
                .position(|k| array_index(k).is_some_and(|m| m > n));
            let at = at.unwrap_or(self.integer_keys);
            self.entries.shift_insert(at, key, value);
            self.integer_keys += 1;
        } else {
            self.entries.insert(key, value);
        }
        None
    }

    /// Removes `key`, returning its value. The other keys keep their order.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let (i, _, value) = self.entries.shift_remove_full(key)?;
        if i < self.integer_keys {
            self.integer_keys -= 1;
        }
        Some(value)
    }

    /// The entries, in order.
    pub fn iter(&self) -> indexmap::map::Iter<'_, String, Value> {
        self.entries.iter()
    }

    pub fn keys(&self) -> indexmap::map::Keys<'_, String, Value> {
        self.entries.keys()
    }

    pub fn values(&self) -> indexmap::map::Values<'_, String, Value> {
        self.entries.values()
    }

    /// The entry at `index`, in order.
    pub fn get_index(&self, index: usize) -> Option<(&String, &Value)> {
        self.entries.get_index(index)
    }
}

impl fmt::Debug for Object {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.entries.iter()).finish()
    }
}

impl<'a> IntoIterator for &'a Object {
    type Item = (&'a String, &'a Value);
    type IntoIter = indexmap::map::Iter<'a, String, Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

impl IntoIterator for Object {
    type Item = (String, Value);
    type IntoIter = indexmap::map::IntoIter<String, Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<K: Into<String>> FromIterator<(K, Value)> for Object {
    /// A key that repeats keeps its first place and its last value, as in a
    /// JS object literal.
    fn from_iter<I: IntoIterator<Item = (K, Value)>>(iter: I) -> Self {
        let mut o = Object::new();
        for (k, v) in iter {
            o.insert(k, v);
        }
        o
    }
}

/// The value of `key` if it is an array index (a canonical integer below
/// 2^32 - 1), which JS orders before other property keys.
pub(crate) fn array_index(key: &str) -> Option<u32> {
    let bytes = key.as_bytes();
    let (&first, rest) = bytes.split_first()?;
    if !first.is_ascii_digit() || bytes.len() > 10 || (first == b'0' && !rest.is_empty()) {
        return None;
    }
    let mut n = u64::from(first - b'0');
    for &c in rest {
        if !c.is_ascii_digit() {
            return None;
        }
        n = n * 10 + u64::from(c - b'0');
    }
    (n < 0xFFFF_FFFF).then_some(n as u32)
}

/// Builds a [`Value`] from JSON-like syntax, for tests and fixtures:
/// `json!({"a": [1, null, "x"]})`.
#[macro_export]
macro_rules! json {
    (null) => { $crate::json::Value::Null };
    ([ $($e:tt),* $(,)? ]) => { $crate::json::Value::Array(vec![ $( $crate::json!($e) ),* ]) };
    ({ $($k:literal : $v:tt),* $(,)? }) => {{
        #[allow(unused_mut)]
        let mut o = $crate::json::Object::new();
        $( o.insert($k, $crate::json!($v)); )*
        $crate::json::Value::Object(o)
    }};
    ($e:expr) => { $crate::json::Value::from($e) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_follow_js_property_order() {
        let mut o = Object::new();
        for k in ["b", "10", "a", "2", "01", "4294967295", "4294967294", "0"] {
            o.insert(k, Value::Null);
        }
        let keys: Vec<_> = o.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["0", "2", "10", "4294967294", "b", "a", "01", "4294967295"]
        );
        o.remove("2");
        o.insert("3", Value::Null);
        let keys: Vec<_> = o.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["0", "3", "10", "4294967294", "b", "a", "01", "4294967295"]
        );
    }
}
