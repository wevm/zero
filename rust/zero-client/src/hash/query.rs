//! Query identities (`packages/zero-protocol/src/query-hash-visitor.ts`).
//!
//! The JS implementation folds an AST (or a name and args) into two xxHash32
//! lanes word by word. These digests are the query IDs the client registers,
//! so they have to match exactly.

use super::xxhash32::{PRIME1, PRIME5, avalanche, round};
use crate::ast::*;
use crate::json::{Value, number};
use crate::strings;

const TAG_NULL: u32 = 0x1001;
const TAG_FALSE: u32 = 0x1002;
const TAG_TRUE: u32 = 0x1003;
const TAG_INT: u32 = 0x1004;
const TAG_FLOAT: u32 = 0x1005;
const TAG_ARR: u32 = 0x1007;
const TAG_OBJ: u32 = 0x1008;
const TAG_END: u32 = 0x1009;
const TAG_UNDEF: u32 = 0x100a;

const TAG_LITERAL: u32 = 0x2001;
const TAG_COLUMN: u32 = 0x2002;
const TAG_STATIC: u32 = 0x2003;
const TAG_SIMPLE: u32 = 0x2004;
const TAG_SUBQUERY: u32 = 0x2005;
const TAG_AND: u32 = 0x2006;
const TAG_OR: u32 = 0x2007;
const TAG_FORMAT: u32 = 0x2008;
const TAG_NAME_ARGS: u32 = 0x2009;
const TAG_SYSTEM_PERMISSIONS: u32 = 0x200a;
const TAG_SYSTEM_CLIENT: u32 = 0x200b;
const TAG_SYSTEM_TEST: u32 = 0x200c;
const TAG_JUNCTION: u32 = 0x200d;
const TAG_AGGREGATE: u32 = 0x200e;
const TAG_JSON: u32 = 0x200f;
const TAG_GROUP_BY: u32 = 0x2010;
const TAG_DISTINCT: u32 = 0x2011;

const STR_MARK: u32 = 0x4000_0000;

/// `hashOfAST`: the hash of `ast` after normalizing it.
pub fn hash_of_ast(ast: &Ast) -> String {
    hash_of_normalized_ast(&ast.normalized())
}

/// The hash of an AST that is already normalized.
pub fn hash_of_normalized_ast(ast: &Ast) -> String {
    let mut s = State::new();
    s.ast(ast);
    s.finish()
}

/// `hashOfNameAndArgs`: the identity of a named (custom) query.
pub fn hash_of_name_and_args(name: &str, args: &[Value]) -> String {
    let mut s = State::new();
    s.mix(TAG_NAME_ARGS);
    s.string(name);
    s.array(args);
    s.finish()
}

/// `hashOfQueryInternals`: the identity of a query as the client sees it.
pub fn hash_of_query_internals(
    ast: &Ast,
    format: &Value,
    system: System,
    current_junction: Option<&str>,
    custom_query: Option<(&str, Option<&[Value]>)>,
) -> String {
    let mut s = State::new();
    s.ast(ast);
    s.mix(TAG_FORMAT);
    s.value(format);
    s.system(system);
    match current_junction {
        Some(j) => {
            s.mix(TAG_JUNCTION);
            s.string(j);
        }
        None => s.mix(TAG_UNDEF),
    }
    match custom_query {
        Some((name, args)) => {
            s.mix(TAG_NAME_ARGS);
            s.string(name);
            if let Some(args) = args {
                s.array(args);
            }
        }
        None => s.mix(TAG_UNDEF),
    }
    s.finish()
}

struct State {
    h1: u32,
    h2: u32,
}

impl State {
    fn new() -> Self {
        State {
            h1: PRIME5,
            h2: PRIME1,
        }
    }

    fn mix(&mut self, w: u32) {
        self.h1 = round(self.h1, w);
        self.h2 = round(self.h2, w);
    }

    /// A JS number's `| 0`, for counts and lengths.
    fn count(&mut self, n: usize) {
        self.mix(n as u32);
    }

    fn string(&mut self, s: &str) {
        let units: Vec<u16> = s.encode_utf16().collect();
        self.mix(units.len() as u32 | STR_MARK);
        for pair in units.chunks(2) {
            self.mix(u32::from(pair[0]) | pair.get(1).map_or(0, |&u| u32::from(u) << 16));
        }
    }

    fn number(&mut self, n: f64) {
        if number::is_int32(n) {
            self.mix(n as i32 as u32);
        } else {
            let bits = n.to_bits(); // little-endian words: low first
            self.mix(bits as u32);
            self.mix((bits >> 32) as u32);
        }
    }

    fn finish(&self) -> String {
        format!(
            "{}{}",
            base36(avalanche(self.h1)),
            base36(avalanche(self.h2))
        )
    }

    fn array(&mut self, a: &[Value]) {
        self.mix(TAG_ARR);
        for e in a {
            self.value(e);
        }
        self.mix(TAG_END);
    }

    fn value(&mut self, v: &Value) {
        match v {
            Value::String(s) => self.string(s),
            // NaN and Infinity are null on the wire (JSON.stringify writes
            // null).
            Value::Number(n) if !n.is_finite() => self.mix(TAG_NULL),
            Value::Number(n) => {
                self.mix(if number::is_int32(*n) {
                    TAG_INT
                } else {
                    TAG_FLOAT
                });
                self.number(*n);
            }
            Value::Bool(b) => self.mix(if *b { TAG_TRUE } else { TAG_FALSE }),
            Value::Null => self.mix(TAG_NULL),
            Value::Array(a) => self.array(a),
            Value::Object(o) => {
                self.mix(TAG_OBJ);
                // `Object.keys(v).sort()`: by UTF-16 code unit.
                let mut entries: Vec<_> = o.iter().collect();
                entries.sort_by(|a, b| strings::utf16(a.0, b.0));
                for (k, v) in entries {
                    self.string(k);
                    self.value(v);
                }
                self.mix(TAG_END);
            }
        }
    }

    fn optional_string(&mut self, s: Option<&str>) {
        match s {
            Some(s) => self.string(s),
            None => self.mix(TAG_UNDEF),
        }
    }

    fn system(&mut self, system: System) {
        self.mix(match system {
            System::Permissions => TAG_SYSTEM_PERMISSIONS,
            System::Client => TAG_SYSTEM_CLIENT,
            System::Test => TAG_SYSTEM_TEST,
        });
    }

    fn optional_bool(&mut self, b: Option<bool>) {
        self.mix(match b {
            None => TAG_UNDEF,
            Some(true) => TAG_TRUE,
            Some(false) => TAG_FALSE,
        });
    }

    fn compound_key(&mut self, k: &[String]) {
        self.count(k.len());
        for s in k {
            self.string(s);
        }
    }

    fn path(&mut self, path: &[JsonPathSegment]) {
        self.array(
            &path
                .iter()
                .map(JsonPathSegment::to_json)
                .collect::<Vec<_>>(),
        );
    }

    fn value_position(&mut self, v: &ValuePosition) {
        match v {
            ValuePosition::Literal(value) => {
                self.mix(TAG_LITERAL);
                self.value(value);
            }
            ValuePosition::Column(name) => {
                self.mix(TAG_COLUMN);
                self.string(name);
            }
            // The wrapped column, then the path as a value.
            ValuePosition::Json { column, path } => {
                self.mix(TAG_JSON);
                self.mix(TAG_COLUMN);
                self.string(column);
                self.path(path);
            }
            ValuePosition::Static { anchor, field } => {
                self.mix(TAG_STATIC);
                self.string(anchor.as_str());
                match field {
                    StaticField::Single(f) => self.string(f),
                    StaticField::Path(p) => self.compound_key(p),
                }
            }
        }
    }

    fn condition(&mut self, c: &Condition) {
        match c {
            Condition::Simple { op, left, right } => {
                self.mix(TAG_SIMPLE);
                self.string(op.as_str());
                self.value_position(left);
                self.value_position(right);
            }
            Condition::CorrelatedSubquery {
                related,
                op,
                flip,
                scalar,
            } => {
                self.mix(TAG_SUBQUERY);
                self.string(op.as_str());
                self.optional_bool(*flip);
                self.optional_bool(*scalar);
                self.correlated_subquery(related);
            }
            Condition::And(conditions) | Condition::Or(conditions) => {
                self.mix(if matches!(c, Condition::And(_)) {
                    TAG_AND
                } else {
                    TAG_OR
                });
                self.count(conditions.len());
                for cond in conditions {
                    self.condition(cond);
                }
            }
        }
    }

    fn correlated_subquery(&mut self, csq: &CorrelatedSubquery) {
        self.compound_key(&csq.correlation.parent_field);
        self.compound_key(&csq.correlation.child_field);
        self.optional_bool(csq.hidden);
        match csq.system {
            Some(system) => self.system(system),
            None => self.mix(TAG_UNDEF),
        }
        // Before the subquery, so an absent aggregate, which writes nothing,
        // is never confused with a present one.
        self.aggregate(csq.aggregate.as_ref());
        self.ast(&csq.subquery);
    }

    /// An absent aggregate, path, or distinct writes nothing, so the hashes
    /// of queries without them are what they were before they existed.
    fn aggregate(&mut self, aggregate: Option<&Aggregate>) {
        let Some(a) = aggregate else { return };
        self.mix(TAG_AGGREGATE);
        self.string(a.fn_.as_str());
        self.optional_string(a.field.as_deref());
        if let Some(path) = &a.path {
            self.mix(TAG_JSON);
            self.path(path);
        }
        if let Some(distinct) = &a.distinct {
            self.mix(TAG_DISTINCT);
            self.compound_key(distinct);
        }
    }

    fn ast(&mut self, ast: &Ast) {
        self.mix(TAG_OBJ);
        self.optional_string(ast.schema.as_deref());
        self.string(&ast.table);
        self.optional_string(ast.alias.as_deref());
        match &ast.where_ {
            Some(w) => self.condition(w),
            None => self.mix(TAG_UNDEF),
        }
        match &ast.related {
            Some(related) => {
                self.count(related.len());
                for r in related {
                    self.correlated_subquery(r);
                }
            }
            None => self.mix(TAG_UNDEF),
        }
        match &ast.start {
            Some(start) => {
                self.mix(if start.exclusive { TAG_TRUE } else { TAG_FALSE });
                self.value(&Value::Object(start.row.clone()));
            }
            None => self.mix(TAG_UNDEF),
        }
        match ast.limit {
            Some(limit) => {
                self.mix(TAG_INT);
                self.number(limit);
            }
            None => self.mix(TAG_UNDEF),
        }
        match &ast.order_by {
            Some(o) => {
                self.count(o.len());
                for part in o {
                    self.string(&part.field);
                    self.mix(if part.direction == Direction::Asc {
                        TAG_TRUE
                    } else {
                        TAG_FALSE
                    });
                }
            }
            None => self.mix(TAG_UNDEF),
        }
        self.aggregate(ast.aggregate.as_ref());
        if let Some(group_by) = &ast.group_by {
            self.mix(TAG_GROUP_BY);
            self.compound_key(group_by);
        }
        self.mix(TAG_END);
    }
}

/// Base 36, padded to 7 digits.
fn base36(mut v: u32) -> String {
    let mut digits = [b'0'; 7];
    let mut i = 7;
    while v > 0 {
        i -= 1;
        digits[i] = b"0123456789abcdefghijklmnopqrstuvwxyz"[(v % 36) as usize];
        v /= 36;
    }
    String::from_utf8(digits.to_vec()).expect("ASCII")
}
