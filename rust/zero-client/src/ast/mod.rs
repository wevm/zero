//! The ZQL abstract syntax tree (`packages/zero-protocol/src/ast.ts`).

mod coding;
mod normalize;
pub mod strict;
mod zql;

pub(crate) use coding::is_homogeneous;
pub(crate) use normalize::cmp_related;
pub use normalize::normalize_condition;
pub use zql::ast_to_zql;

use crate::json::{Object, Value};

/// A query: a table, its filters, relationships, order, and bounds.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Ast {
    pub schema: Option<String>,
    pub table: String,
    /// Set on subqueries, to tell apart two subqueries of the same table.
    pub alias: Option<String>,
    pub where_: Option<Condition>,
    pub related: Option<Vec<CorrelatedSubquery>>,
    pub start: Option<Bound>,
    pub limit: Option<f64>,
    pub order_by: Option<Vec<OrderPart>>,
    /// Reduces the query to one aggregate over the rows left after `where`,
    /// `start`, and `limit`; with `group_by`, to one per group.
    pub aggregate: Option<Aggregate>,
    /// Groups the rows that pass `where` by these columns: alone a
    /// `SELECT DISTINCT` of them, with `aggregate` that aggregate per group.
    pub group_by: Option<Vec<String>>,
}

impl Ast {
    pub fn new(table: impl Into<String>) -> Self {
        Ast {
            table: table.into(),
            ..Ast::default()
        }
    }
}

macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident = $s:literal),* $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name { $(#[doc = concat!("`", $s, "`")] $variant),* }

        impl $name {
            /// Every value, in declaration order.
            pub const ALL: &[$name] = &[$($name::$variant),*];

            /// The value as JS writes it.
            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $s),* }
            }

            /// The value JS writes as `s`, if any.
            pub fn parse(s: &str) -> Option<$name> {
                match s { $($s => Some($name::$variant),)* _ => None }
            }
        }
    };
}
pub(crate) use string_enum;

string_enum!(
    /// An aggregate function (`AggregateFunction`).
    AggregateFunction { Count = "count", Sum = "sum", Avg = "avg", Min = "min", Max = "max" }
);

string_enum!(System { Permissions = "permissions", Client = "client", Test = "test" });

string_enum!(Direction { Asc = "asc", Desc = "desc" });

string_enum!(SimpleOperator {
    Eq = "=",
    Ne = "!=",
    Is = "IS",
    IsNot = "IS NOT",
    Lt = "<",
    Gt = ">",
    Le = "<=",
    Ge = ">=",
    Like = "LIKE",
    NotLike = "NOT LIKE",
    Ilike = "ILIKE",
    NotIlike = "NOT ILIKE",
    In = "IN",
    NotIn = "NOT IN",
});

string_enum!(CorrelatedSubqueryOperator { Exists = "EXISTS", NotExists = "NOT EXISTS" });

string_enum!(Anchor { AuthData = "authData", PreMutationRow = "preMutationRow" });

/// One step of a JSON path: an object key or an array index.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum JsonPathSegment {
    Key(String),
    Index(u32),
}

impl JsonPathSegment {
    pub fn to_json(&self) -> Value {
        match self {
            JsonPathSegment::Key(k) => Value::String(k.clone()),
            JsonPathSegment::Index(i) => Value::Number(f64::from(*i)),
        }
    }
}

/// `MAX_JSON_PATH_INDEX`: the largest array index a path may hold.
pub const MAX_JSON_PATH_INDEX: u32 = i32::MAX as u32;

/// An aggregate of a query or a relationship (`Aggregate`).
#[derive(Clone, Debug, PartialEq)]
pub struct Aggregate {
    pub fn_: AggregateFunction,
    /// The column to aggregate; `None` for `count`.
    pub field: Option<String>,
    /// A path into `field`, then a `json` column, to a scalar leaf.
    pub path: Option<Vec<JsonPathSegment>>,
    /// For `count`: counts the distinct combinations of these columns.
    pub distinct: Option<Vec<String>>,
}

impl Aggregate {
    pub fn new(fn_: AggregateFunction) -> Self {
        Aggregate {
            fn_,
            field: None,
            path: None,
            distinct: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bound {
    /// The cursor's columns: those the query orders by.
    pub row: Object,
    pub exclusive: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OrderPart {
    pub field: String,
    pub direction: Direction,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StaticField {
    Single(String),
    Path(Vec<String>),
}

impl StaticField {
    pub fn to_json(&self) -> Value {
        match self {
            StaticField::Single(s) => Value::String(s.clone()),
            StaticField::Path(p) => crate::decode::strings_json(p),
        }
    }
}

/// One side of a simple condition. A literal is a string, number, boolean,
/// null, or an array of strings, numbers, and booleans.
#[derive(Clone, Debug, PartialEq)]
pub enum ValuePosition {
    Literal(Value),
    Column(String),
    /// A value inside the `json` column `column`, at `path`.
    Json {
        column: String,
        path: Vec<JsonPathSegment>,
    },
    Static {
        anchor: Anchor,
        field: StaticField,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Condition {
    Simple {
        op: SimpleOperator,
        left: ValuePosition,
        right: ValuePosition,
    },
    And(Vec<Condition>),
    Or(Vec<Condition>),
    CorrelatedSubquery {
        related: CorrelatedSubquery,
        op: CorrelatedSubqueryOperator,
        flip: Option<bool>,
        scalar: Option<bool>,
    },
}

impl Condition {
    /// The conditions of a conjunction or disjunction.
    pub fn conditions(&self) -> Option<&[Condition]> {
        match self {
            Condition::And(c) | Condition::Or(c) => Some(c),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Correlation {
    pub parent_field: Vec<String>,
    pub child_field: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CorrelatedSubquery {
    pub correlation: Correlation,
    pub subquery: Box<Ast>,
    pub system: Option<System>,
    /// Hidden hops (junction tables) are left out of the output view; their
    /// children are not.
    pub hidden: Option<bool>,
    /// Reduces the subquery to one aggregate per parent row.
    pub aggregate: Option<Aggregate>,
}

/// `jsonValueAtPath`: the value at `path` in `value`, strictly: a key reads
/// an object's own key and an index an array's element. Any other step, and
/// a missing value, is null.
pub fn json_value_at<'a>(path: &[JsonPathSegment], value: Option<&'a Value>) -> &'a Value {
    static NULL: Value = Value::Null;
    let mut v = value.unwrap_or(&NULL);
    for segment in path {
        v = match (v, segment) {
            (Value::Object(o), JsonPathSegment::Key(k)) => o.get(k).unwrap_or(&NULL),
            (Value::Array(a), JsonPathSegment::Index(i)) => a.get(*i as usize).unwrap_or(&NULL),
            _ => &NULL,
        };
    }
    v
}

/// `formatJsonPathReference`: `metadata.tags[0]`.
pub fn format_json_path(column: &str, path: &[JsonPathSegment]) -> String {
    let mut out = column.to_owned();
    for segment in path {
        match segment {
            JsonPathSegment::Key(k) => {
                out.push('.');
                out.push_str(k);
            }
            JsonPathSegment::Index(i) => out.push_str(&format!("[{i}]")),
        }
    }
    out
}
