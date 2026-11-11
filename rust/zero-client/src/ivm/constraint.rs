//! Column values a fetch must match (`packages/zql/src/ivm/constraint.ts`).

use super::{Row, values_equal};
use crate::ast::{Condition, SimpleOperator, ValuePosition};
use crate::json::Value;
use crate::strings;

/// Keys keep insertion order, like the JS object: a memory source builds its
/// index sort from it. A value is `None` where JS has `undefined`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Constraint {
    pub entries: Vec<(String, Option<Value>)>,
}

impl Constraint {
    pub fn new(entries: Vec<(String, Option<Value>)>) -> Self {
        Constraint { entries }
    }

    pub fn keys(&self) -> Vec<&str> {
        self.entries.iter().map(|(k, _)| k.as_str()).collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn has(&self, key: &str) -> bool {
        self.entries.iter().any(|(k, _)| k == key)
    }

    /// The value for `key`; `None` when absent or `undefined`.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.as_ref())
    }

    pub fn set(&mut self, key: &str, value: Option<Value>) {
        match self.entries.iter_mut().find(|(k, _)| k == key) {
            Some(e) => e.1 = value,
            None => self.entries.push((key.to_owned(), value)),
        }
    }

    /// `{...self, ...other}`.
    pub fn merging(&self, other: &Constraint) -> Constraint {
        let mut result = self.clone();
        for (k, v) in &other.entries {
            result.set(k, v.clone());
        }
        result
    }

    /// `constraintMatchesRow`.
    pub fn matches(&self, row: &Row) -> bool {
        self.entries
            .iter()
            .all(|(k, v)| values_equal(row.get(k), v.as_ref()))
    }

    /// `constraintsAreCompatible`: no shared key has different values.
    pub fn is_compatible(&self, other: &Constraint) -> bool {
        self.entries
            .iter()
            .all(|(k, v)| !other.has(k) || values_equal(v.as_ref(), other.get(k)))
    }

    /// `constraintMatchesPrimaryKey`.
    pub fn matches_primary_key(&self, primary_key: &[String]) -> bool {
        key_matches_primary_key(&self.keys(), primary_key)
    }

    /// `{key: row[key]}` for each of `keys`.
    pub fn from_row(row: &Row, keys: &[String]) -> Constraint {
        Constraint {
            entries: keys
                .iter()
                .map(|k| (k.clone(), row.get(k).cloned()))
                .collect(),
        }
    }
}

/// `keyMatchesPrimaryKey`. The primary key is sorted; `key` need not be.
pub fn key_matches_primary_key(key: &[&str], primary_key: &[String]) -> bool {
    if key.len() != primary_key.len() {
        return false;
    }
    let mut sorted = key.to_vec();
    sorted.sort_by(|a, b| strings::utf16(a, b));
    sorted.iter().zip(primary_key).all(|(a, b)| *a == b)
}

/// `pullSimpleAndComponents`: the top-level AND conjuncts that are simple.
pub fn pull_simple_and_components(condition: &Condition) -> Vec<&Condition> {
    match condition {
        Condition::And(c) => c.iter().flat_map(pull_simple_and_components).collect(),
        Condition::Simple { .. } => vec![condition],
        Condition::Or(c) if c.len() == 1 => pull_simple_and_components(&c[0]),
        _ => Vec::new(),
    }
}

/// `primaryKeyConstraintFromFilters`: the primary key lookup the filters
/// amount to, if they do.
pub fn primary_key_constraint_from_filters(
    condition: Option<&Condition>,
    primary_key: &[String],
) -> Option<Constraint> {
    let conditions = pull_simple_and_components(condition?);
    if conditions.is_empty() {
        return None;
    }
    let mut ret = Constraint::default();
    for c in conditions {
        let Condition::Simple {
            op: SimpleOperator::Eq,
            left: ValuePosition::Column(name),
            right,
        } = c
        else {
            continue;
        };
        let ValuePosition::Literal(value) = right else {
            panic!("extractColumn: expected right side to be literal");
        };
        if primary_key.contains(name) {
            ret.set(name, Some(value.clone()));
        }
    }
    (ret.len() == primary_key.len()).then_some(ret)
}
