//! [`OrderedMap`]: a map with string keys that keeps them in insertion order,
//! so the crate needs no third-party map.

use std::collections::HashMap;
use std::fmt;

/// Maps up to this many keys are searched linearly, which beats hashing for
/// the few columns of a row; larger ones keep a hash index.
const INDEX_THRESHOLD: usize = 16;

/// A map from strings to `V` that keeps its keys in the order they were
/// inserted. Lookups are O(1): small maps scan their few entries, and larger
/// ones keep a hash index of positions. Inserting or removing at a position
/// shifts the entries after it, in O(n).
///
/// Two maps are equal when they have the same keys with equal values, in
/// any order.
#[derive(Clone)]
pub struct OrderedMap<V> {
    keys: Keys,
    values: Vec<V>,
}

/// The keys of an [`OrderedMap`] or a JSON object, in order, apart from the
/// values, so that objects can share them.
#[derive(Clone, Default)]
pub(crate) struct Keys {
    pub(crate) names: Vec<String>,
    /// Each key's position in `names`, once there are more than
    /// `INDEX_THRESHOLD`.
    index: Option<HashMap<String, usize>>,
}

impl Keys {
    pub(crate) fn position(&self, key: &str) -> Option<usize> {
        match &self.index {
            Some(index) => index.get(key).copied(),
            None => self.names.iter().position(|k| k == key),
        }
    }

    /// Inserts `key`, which must not be present yet, at position `at`.
    pub(crate) fn insert(&mut self, at: usize, key: String) {
        if let Some(index) = &mut self.index {
            if at < self.names.len() {
                for i in index.values_mut().filter(|i| **i >= at) {
                    *i += 1;
                }
            }
            index.insert(key.clone(), at);
        }
        self.names.insert(at, key);
        if self.index.is_none() && self.names.len() > INDEX_THRESHOLD {
            self.index = Some(
                self.names
                    .iter()
                    .enumerate()
                    .map(|(i, k)| (k.clone(), i))
                    .collect(),
            );
        }
    }

    /// Removes the key at `at`.
    pub(crate) fn remove(&mut self, at: usize) {
        let key = self.names.remove(at);
        if let Some(index) = &mut self.index {
            index.remove(&key);
            for i in index.values_mut().filter(|i| **i > at) {
                *i -= 1;
            }
        }
    }
}

impl<V> Default for OrderedMap<V> {
    fn default() -> Self {
        OrderedMap {
            keys: Keys::default(),
            values: Vec::new(),
        }
    }
}

impl<V> OrderedMap<V> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        OrderedMap {
            keys: Keys {
                names: Vec::with_capacity(capacity),
                index: None,
            },
            values: Vec::with_capacity(capacity),
        }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// The position of `key`.
    pub fn position(&self, key: &str) -> Option<usize> {
        self.keys.position(key)
    }

    pub fn get(&self, key: &str) -> Option<&V> {
        self.position(key).map(|i| &self.values[i])
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.position(key).map(|i| &mut self.values[i])
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.position(key).is_some()
    }

    /// The entry at `index`, in order.
    pub fn get_index(&self, index: usize) -> Option<(&String, &V)> {
        Some((self.keys.names.get(index)?, &self.values[index]))
    }

    /// Sets `key` to `value`, returning the value it replaced. A new key goes
    /// last; an existing one keeps its place.
    pub fn insert(&mut self, key: impl Into<String>, value: V) -> Option<V> {
        let key = key.into();
        if let Some(i) = self.position(&key) {
            return Some(std::mem::replace(&mut self.values[i], value));
        }
        let at = self.values.len();
        self.insert_new(at, key, value);
        None
    }

    /// Inserts `key`, which must not be present yet, at position `at`,
    /// shifting the entries after it.
    pub fn insert_at(&mut self, at: usize, key: impl Into<String>, value: V) {
        let key = key.into();
        assert!(
            !self.contains_key(&key),
            "OrderedMap::insert_at: key present"
        );
        self.insert_new(at, key, value);
    }

    fn insert_new(&mut self, at: usize, key: String, value: V) {
        self.keys.insert(at, key);
        self.values.insert(at, value);
    }

    /// Removes `key`, returning its position and value. The other keys keep
    /// their order.
    pub fn remove_full(&mut self, key: &str) -> Option<(usize, V)> {
        let at = self.position(key)?;
        self.keys.remove(at);
        Some((at, self.values.remove(at)))
    }

    /// Removes `key`, returning its value. The other keys keep their order.
    pub fn remove(&mut self, key: &str) -> Option<V> {
        self.remove_full(key).map(|(_, v)| v)
    }

    /// The entries, in order.
    pub fn iter(&self) -> Iter<'_, V> {
        Iter::new(&self.keys.names, &self.values)
    }

    /// The entries, in order, with mutable values.
    pub fn iter_mut(&mut self) -> impl ExactSizeIterator<Item = (&String, &mut V)> {
        self.keys.names.iter().zip(self.values.iter_mut())
    }

    pub fn keys(&self) -> impl DoubleEndedIterator<Item = &String> + ExactSizeIterator + Clone {
        self.keys.names.iter()
    }

    pub fn values(&self) -> impl DoubleEndedIterator<Item = &V> + ExactSizeIterator + Clone {
        self.values.iter()
    }

    pub fn values_mut(&mut self) -> impl ExactSizeIterator<Item = &mut V> {
        self.values.iter_mut()
    }
}

impl<V: PartialEq> PartialEq for OrderedMap<V> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().all(|(k, v)| other.get(k) == Some(v))
    }
}

impl<V: fmt::Debug> fmt::Debug for OrderedMap<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<K: Into<String>, V> FromIterator<(K, V)> for OrderedMap<V> {
    /// A key that repeats keeps its first place and its last value.
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let iter = iter.into_iter();
        let mut map = OrderedMap::with_capacity(iter.size_hint().0);
        for (k, v) in iter {
            map.insert(k, v);
        }
        map
    }
}

/// The entries of an [`OrderedMap`], in order.
#[derive(Clone)]
pub struct Iter<'a, V>(std::iter::Zip<std::slice::Iter<'a, String>, std::slice::Iter<'a, V>>);

impl<'a, V> Iter<'a, V> {
    /// The entries of `keys` with `values`, which have the same length.
    pub(crate) fn new(keys: &'a [String], values: &'a [V]) -> Self {
        Iter(keys.iter().zip(values))
    }
}

impl<'a, V> Iterator for Iter<'a, V> {
    type Item = (&'a String, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl<V> DoubleEndedIterator for Iter<'_, V> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.0.next_back()
    }
}

impl<V> ExactSizeIterator for Iter<'_, V> {}

/// The entries of an [`OrderedMap`], in order, by value.
pub struct IntoIter<V>(std::iter::Zip<std::vec::IntoIter<String>, std::vec::IntoIter<V>>);

impl<V> IntoIter<V> {
    /// The entries of `keys` with `values`, which have the same length.
    pub(crate) fn new(keys: Vec<String>, values: Vec<V>) -> Self {
        IntoIter(keys.into_iter().zip(values))
    }
}

impl<V> Iterator for IntoIter<V> {
    type Item = (String, V);

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl<V> DoubleEndedIterator for IntoIter<V> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.0.next_back()
    }
}

impl<V> ExactSizeIterator for IntoIter<V> {}

impl<'a, V> IntoIterator for &'a OrderedMap<V> {
    type Item = (&'a String, &'a V);
    type IntoIter = Iter<'a, V>;

    fn into_iter(self) -> Iter<'a, V> {
        self.iter()
    }
}

impl<V> IntoIterator for OrderedMap<V> {
    type Item = (String, V);
    type IntoIter = IntoIter<V>;

    fn into_iter(self) -> IntoIter<V> {
        IntoIter::new(self.keys.names, self.values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Checks every lookup against the entries' own order, across the size
    /// where the hash index starts.
    fn check(map: &OrderedMap<usize>) {
        for (i, (k, v)) in map.iter().enumerate() {
            assert_eq!(map.position(k), Some(i), "{k}");
            assert_eq!(map.get(k), Some(v));
        }
        assert_eq!(map.get("missing"), None);
    }

    #[test]
    fn keeps_order_and_positions_through_inserts_and_removes() {
        let mut map = OrderedMap::new();
        for i in 0..20 {
            map.insert(format!("k{i}"), i);
            check(&map);
        }
        assert_eq!(map.insert("k3", 30), Some(3));
        map.insert_at(0, "first", 100);
        map.insert_at(5, "middle", 101);
        check(&map);
        assert_eq!(map.remove_full("k0"), Some((1, 0)));
        assert_eq!(map.remove("middle"), Some(101));
        check(&map);
        for i in 1..20 {
            map.remove(&format!("k{i}"));
            check(&map);
        }
        assert_eq!(map.keys().collect::<Vec<_>>(), ["first"]);
        map.insert("again", 1);
        check(&map);
    }

    #[test]
    fn equality_ignores_order() {
        let a: OrderedMap<i32> = [("x", 1), ("y", 2)].into_iter().collect();
        let b: OrderedMap<i32> = [("y", 2), ("x", 1)].into_iter().collect();
        let c: OrderedMap<i32> = [("y", 2), ("x", 3)].into_iter().collect();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
