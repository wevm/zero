use crate::map::{self, Keys};
use std::cell::Cell;
use std::fmt;
use std::sync::{Arc, LazyLock};

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
///
/// The keys are kept apart from the values, in a list that objects with the
/// same keys in the same order can share: the rows of a table share one, the
/// table's layout, as JS engines' hidden classes do, so reading a row's
/// column reaches its value without touching a copy of the column's name.
/// Cloning an object is cheap: copies share their values until one of them
/// is changed.
#[derive(Clone)]
pub struct Object {
    keys: Arc<Keys>,
    values: Arc<[Value]>,
}

impl Default for Object {
    /// An empty object, which allocates nothing.
    fn default() -> Self {
        static KEYS: LazyLock<Arc<Keys>> = LazyLock::new(Arc::default);
        static VALUES: LazyLock<Arc<[Value]>> = LazyLock::new(|| Arc::new([]));
        Object {
            keys: KEYS.clone(),
            values: VALUES.clone(),
        }
    }
}

/// An object being built, before its keys and values are shared.
#[derive(Default)]
pub(crate) struct ObjectBuilder {
    keys: Keys,
    values: Vec<Value>,
    /// The keys of another object, which this one has had so far: its keys
    /// are the first `values.len()` of them, and `keys` is empty. The parser
    /// follows a recent object's keys, so that the rows of a table share
    /// their keys without the keys being allocated.
    following: Option<Arc<Keys>>,
}

impl ObjectBuilder {
    /// The keys so far and the next key of the layout it follows, while it
    /// follows one or has no keys yet.
    pub(crate) fn prefix(&self) -> Option<(&[String], Option<&String>)> {
        let n = self.values.len();
        match &self.following {
            Some(layout) => Some((&layout.names[..n], layout.names.get(n))),
            None if n == 0 => Some((&[], None)),
            None => None,
        }
    }

    /// Follows `layout`, whose first keys are the keys so far (see
    /// `following`).
    pub(crate) fn follow(&mut self, layout: Arc<Keys>) {
        debug_assert!(
            self.prefix()
                .is_some_and(|(p, _)| *p == layout.names[..p.len()])
        );
        self.values
            .reserve(layout.names.len().saturating_sub(self.values.len()));
        self.following = Some(layout);
    }

    /// Sets the next key of the layout it follows to `value`.
    pub(crate) fn push_expected(&mut self, value: Value) {
        debug_assert!(self.prefix().is_some_and(|(_, next)| next.is_some()));
        self.values.push(value);
    }

    /// Stops following a layout: the object gets its own keys.
    fn own_keys(&mut self) {
        if let Some(layout) = self.following.take() {
            for name in &layout.names[..self.values.len()] {
                let at = self.keys.names.len();
                self.keys.insert(at, name.clone());
            }
        }
    }

    /// [`Object::insert`].
    pub(crate) fn insert(&mut self, key: String, value: Value) -> Option<Value> {
        self.own_keys();
        if let Some(i) = self.keys.position(&key) {
            return Some(std::mem::replace(&mut self.values[i], value));
        }
        // An integer-like key goes among the integer-like keys at the front,
        // in numeric order.
        let at = match array_index(&key) {
            Some(n) => self
                .keys
                .names
                .iter()
                .position(|k| array_index(k).is_none_or(|m| m > n))
                .unwrap_or(self.values.len()),
            None => self.values.len(),
        };
        self.keys.insert(at, key);
        self.values.insert(at, value);
        None
    }

    pub(crate) fn build(mut self) -> Object {
        if self.values.is_empty() {
            return Object::new();
        }
        let keys = match self.following.take() {
            Some(layout) if layout.names.len() == self.values.len() => layout,
            layout => {
                self.following = layout;
                self.own_keys();
                Arc::new(self.keys)
            }
        };
        Object {
            keys,
            values: self.values.into(),
        }
    }
}

/// The keys of the objects a parse built last, most recent first, which
/// the next objects follow when their keys match.
#[derive(Default)]
pub(crate) struct RecentKeys(Vec<Arc<Keys>>);

impl RecentKeys {
    const SIZE: usize = 8;

    /// The most recent keys that start with `prefix` and then a key that
    /// `is_next` accepts.
    pub(crate) fn find_next(
        &self,
        prefix: &[String],
        mut is_next: impl FnMut(&str) -> bool,
    ) -> Option<Arc<Keys>> {
        let n = prefix.len();
        self.0
            .iter()
            .find(|k| k.names.len() > n && k.names[..n] == *prefix && is_next(&k.names[n]))
            .cloned()
    }

    /// Makes `object`'s keys the most recent. An object that has the same
    /// keys as a recent one, which it built its own of, shares them.
    pub(crate) fn remember(&mut self, object: &mut Object) {
        if object.is_empty() {
            return;
        }
        let same = self
            .0
            .iter()
            .position(|k| Arc::ptr_eq(k, &object.keys))
            .or_else(|| {
                let i = self.0.iter().position(|k| k.names == object.keys.names)?;
                object.keys = self.0[i].clone();
                Some(i)
            });
        match same {
            Some(i) => self.0[..=i].rotate_right(1),
            None => {
                self.0.truncate(Self::SIZE - 1);
                self.0.insert(0, object.keys.clone());
            }
        }
    }
}

/// The values, moved out when nothing else shares them.
fn into_vec(mut values: Arc<[Value]>) -> Vec<Value> {
    match Arc::get_mut(&mut values) {
        Some(owned) => owned.iter_mut().map(std::mem::take).collect(),
        None => values.to_vec(),
    }
}

impl Object {
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty object. Its values are allocated once it has keys, so the
    /// capacity isn't used.
    pub fn with_capacity(_capacity: usize) -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.keys.position(key).map(|i| &self.values[i])
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        let i = self.keys.position(key)?;
        Some(&mut Arc::make_mut(&mut self.values)[i])
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.keys.position(key).is_some()
    }

    /// Changes the keys, after which the object has its own.
    fn edit<R>(&mut self, f: impl FnOnce(&mut ObjectBuilder) -> R) -> R {
        let Object { keys, values } = std::mem::take(self);
        let mut builder = ObjectBuilder {
            keys: Arc::unwrap_or_clone(keys),
            values: into_vec(values),
            following: None,
        };
        let result = f(&mut builder);
        *self = builder.build();
        result
    }

    /// Sets `key` to `value`, returning the value it replaced. A new key goes
    /// after the others, or among the integer-like keys in numeric order.
    pub fn insert(&mut self, key: impl Into<String>, value: Value) -> Option<Value> {
        let key = key.into();
        if let Some(i) = self.keys.position(&key) {
            return Some(std::mem::replace(
                &mut Arc::make_mut(&mut self.values)[i],
                value,
            ));
        }
        self.edit(|b| b.insert(key, value))
    }

    /// Removes `key`, returning its value. The other keys keep their order.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let i = self.keys.position(key)?;
        self.edit(|b| {
            b.keys.remove(i);
            Some(b.values.remove(i))
        })
    }

    /// The entries, in order.
    pub fn iter(&self) -> map::Iter<'_, Value> {
        map::Iter::new(&self.keys.names, &self.values)
    }

    pub fn keys(&self) -> impl DoubleEndedIterator<Item = &String> + ExactSizeIterator + Clone {
        self.keys.names.iter()
    }

    pub fn values(&self) -> impl DoubleEndedIterator<Item = &Value> + ExactSizeIterator + Clone {
        self.values.iter()
    }

    /// The entry at `index`, in order.
    pub fn get_index(&self, index: usize) -> Option<(&String, &Value)> {
        Some((self.keys.names.get(index)?, &self.values[index]))
    }

    /// [`Object::get`], looking first at the position in `hint`, where the
    /// key was in the last object looked up: the rows of a table have their
    /// columns in the same places, so a comparator or a filter finds a
    /// column without searching the names.
    pub(crate) fn get_hinted(&self, key: &str, hint: &Cell<usize>) -> Option<&Value> {
        if let Some((k, v)) = self.get_index(hint.get())
            && k == key
        {
            return Some(v);
        }
        let i = self.keys.position(key)?;
        hint.set(i);
        Some(&self.values[i])
    }

    /// Shares `layout`'s keys when they are this object's keys in the same
    /// order, so the two keep one list of them. Returns whether the keys
    /// are shared now.
    pub(crate) fn share_keys(&mut self, layout: &Object) -> bool {
        if Arc::ptr_eq(&self.keys, &layout.keys) {
            return true;
        }
        if self.keys.names != layout.keys.names {
            return false;
        }
        self.keys = layout.keys.clone();
        true
    }
}

impl PartialEq for Object {
    fn eq(&self, other: &Self) -> bool {
        if Arc::ptr_eq(&self.keys, &other.keys) {
            return Arc::ptr_eq(&self.values, &other.values) || self.values == other.values;
        }
        self.len() == other.len() && self.iter().all(|(k, v)| other.get(k) == Some(v))
    }
}

impl fmt::Debug for Object {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<'a> IntoIterator for &'a Object {
    type Item = (&'a String, &'a Value);
    type IntoIter = map::Iter<'a, Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl IntoIterator for Object {
    type Item = (String, Value);
    type IntoIter = map::IntoIter<Value>;
    fn into_iter(self) -> Self::IntoIter {
        map::IntoIter::new(Arc::unwrap_or_clone(self.keys).names, into_vec(self.values))
    }
}

impl<K: Into<String>> FromIterator<(K, Value)> for Object {
    /// A key that repeats keeps its first place and its last value, as in a
    /// JS object literal.
    fn from_iter<I: IntoIterator<Item = (K, Value)>>(iter: I) -> Self {
        let mut o = ObjectBuilder::default();
        for (k, v) in iter {
            o.insert(k.into(), v);
        }
        o.build()
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

    #[test]
    fn parsed_objects_with_the_same_keys_share_them() {
        let text = r#"[{"a":1,"b":2},{"a":3,"b":4},{"a":5},{"a":6,"b":7,"c":8},
            {"b":1,"a":2},{"a":1,"a":2,"b":3},{"\u0061":1,"b\"":2},{"1":0,"0":1,"a":2},
            {"a":9,"b":10}]"#;
        let Value::Array(objects) = crate::json::parse(text).unwrap() else {
            panic!("an array");
        };
        let objects: Vec<&Object> = objects.iter().map(|v| v.as_object().unwrap()).collect();
        let entries: Vec<Vec<(&str, f64)>> = objects
            .iter()
            .map(|o| {
                o.iter()
                    .map(|(k, v)| (k.as_str(), v.as_f64().unwrap()))
                    .collect()
            })
            .collect();
        assert_eq!(
            entries,
            [
                vec![("a", 1.0), ("b", 2.0)],
                vec![("a", 3.0), ("b", 4.0)],
                vec![("a", 5.0)],
                vec![("a", 6.0), ("b", 7.0), ("c", 8.0)],
                vec![("b", 1.0), ("a", 2.0)],
                vec![("a", 2.0), ("b", 3.0)],
                vec![("a", 1.0), ("b\"", 2.0)],
                vec![("0", 1.0), ("1", 0.0), ("a", 2.0)],
                vec![("a", 9.0), ("b", 10.0)],
            ]
        );
        let shared = |i: usize, j: usize| Arc::ptr_eq(&objects[i].keys, &objects[j].keys);
        assert!(shared(0, 1) && shared(0, 8) && shared(0, 5));
        assert!(!shared(0, 2) && !shared(0, 3) && !shared(0, 4));
        // A change to an object whose keys are shared leaves the others'.
        let mut changed = objects[1].clone();
        changed.insert("z", Value::Null);
        assert_eq!(objects[0].keys().collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(changed.keys().collect::<Vec<_>>(), ["a", "b", "z"]);
    }

    #[test]
    fn a_change_to_a_copy_leaves_the_original() {
        let mut original = Object::new();
        original.insert("a", Value::from(1.0));
        original.insert("b", Value::from(2.0));
        let changes: [fn(&mut Object); 4] = [
            |o| _ = o.insert("a", Value::from(9.0)),
            |o| _ = o.insert("1", Value::Null),
            |o| _ = o.remove("b"),
            |o| *o.get_mut("a").unwrap() = Value::Null,
        ];
        for change in changes {
            let mut copy = original.clone();
            change(&mut copy);
            assert_ne!(copy, original);
            let keys: Vec<_> = original.iter().collect();
            assert_eq!(
                keys,
                [
                    (&"a".to_owned(), &Value::from(1.0)),
                    (&"b".to_owned(), &Value::from(2.0))
                ]
            );
        }
    }
}
