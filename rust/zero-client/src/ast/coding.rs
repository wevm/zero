//! The JSON form of the AST, following `astSchema`.

use super::*;
use crate::decode::{self, Builder, DecodeError, JsonCodable, Reader, Result, strings_json};

impl JsonCodable for Ast {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "AST")?;
        Ok(Ast {
            schema: r.optional_string("schema")?,
            table: r.string("table")?,
            alias: r.optional_string("alias")?,
            where_: r.optional("where", Condition::from_json)?,
            related: r.optional("related", |v| {
                decode::array(v, CorrelatedSubquery::from_json)
            })?,
            limit: r.optional_number("limit")?,
            order_by: r.optional("orderBy", |v| {
                decode::array(v, |e| match e.as_array().map(Vec::as_slice) {
                    Some([Value::String(field), Value::String(dir)]) => Direction::parse(dir)
                        .map(|direction| OrderPart {
                            field: field.clone(),
                            direction,
                        })
                        .ok_or_else(|| DecodeError::new("Expected [field, 'asc' | 'desc']")),
                    _ => Err(DecodeError::new("Expected [field, 'asc' | 'desc']")),
                })
            })?,
            start: r.optional("start", |v| {
                let b = Reader::new(v, "bound")?;
                Ok(Bound {
                    row: b.required("row", decode::object)?,
                    exclusive: b.bool("exclusive")?,
                })
            })?,
            aggregate: r.optional("aggregate", Aggregate::from_json)?,
            group_by: r.optional("groupBy", |v| {
                let columns = decode::strings(v)?;
                if columns.is_empty() {
                    return Err(DecodeError::new("expected groupBy to name a column"));
                }
                Ok(columns)
            })?,
        })
    }

    fn to_json(&self) -> Value {
        Builder::new()
            .opt("schema", self.schema.clone())
            .set("table", self.table.clone())
            .opt("alias", self.alias.clone())
            .opt("where", self.where_.as_ref().map(Condition::to_json))
            .opt(
                "related",
                self.related
                    .as_ref()
                    .map(|r| Value::Array(r.iter().map(CorrelatedSubquery::to_json).collect())),
            )
            .opt(
                "start",
                self.start.as_ref().map(|b| {
                    Builder::new()
                        .set("row", b.row.clone())
                        .set("exclusive", b.exclusive)
                        .build()
                }),
            )
            .opt("limit", self.limit)
            .opt(
                "orderBy",
                self.order_by.as_ref().map(|o| {
                    Value::Array(
                        o.iter()
                            .map(|p| {
                                Value::Array(vec![
                                    p.field.as_str().into(),
                                    p.direction.as_str().into(),
                                ])
                            })
                            .collect(),
                    )
                }),
            )
            .opt("aggregate", self.aggregate.as_ref().map(Aggregate::to_json))
            .opt("groupBy", self.group_by.as_deref().map(strings_json))
            .build()
    }
}

/// `jsonPathSchema`: a non-empty list of keys and array indices.
pub(crate) fn path_from_json(v: &Value) -> Result<Vec<JsonPathSegment>> {
    let path = decode::array(v, |e| match e {
        Value::String(k) => Ok(JsonPathSegment::Key(k.clone())),
        Value::Number(n)
            if n.trunc() == *n && *n >= 0.0 && *n <= f64::from(MAX_JSON_PATH_INDEX) =>
        {
            Ok(JsonPathSegment::Index(*n as u32))
        }
        Value::Number(_) => Err(DecodeError::new(
            "expected a non-negative integer array index",
        )),
        _ => Err(DecodeError::new("Expected string or number")),
    })?;
    if path.is_empty() {
        return Err(DecodeError::new("expected a non-empty JSON path"));
    }
    Ok(path)
}

pub(crate) fn path_to_json(path: &[JsonPathSegment]) -> Value {
    Value::Array(path.iter().map(JsonPathSegment::to_json).collect())
}

impl JsonCodable for Aggregate {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "aggregate")?;
        let name = r.string("fn")?;
        let fn_ = AggregateFunction::parse(&name).ok_or_else(|| {
            DecodeError::new(format!("Invalid aggregate function \"{name}\"")).at("fn")
        })?;
        let a = Aggregate {
            fn_,
            field: r.optional_string("field")?,
            path: r.optional("path", path_from_json)?,
            distinct: r.optional("distinct", decode::strings)?,
        };
        if a.path.is_some() && (a.field.is_none() || fn_ == AggregateFunction::Count) {
            return Err(DecodeError::new(
                "expected an aggregate JSON path to have a field and not be a count",
            ));
        }
        if let Some(distinct) = &a.distinct
            && (fn_ != AggregateFunction::Count || a.field.is_some() || distinct.is_empty())
        {
            return Err(DecodeError::new(
                "expected an aggregate over distinct columns to be a count of at least one",
            ));
        }
        Ok(a)
    }

    fn to_json(&self) -> Value {
        Builder::new()
            .set("fn", self.fn_.as_str())
            .opt("field", self.field.clone())
            .opt("path", self.path.as_deref().map(path_to_json))
            .opt("distinct", self.distinct.as_deref().map(strings_json))
            .build()
    }
}

impl JsonCodable for CorrelatedSubquery {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "correlated subquery")?;
        Ok(CorrelatedSubquery {
            correlation: r.required("correlation", |v| {
                let c = Reader::new(v, "correlation")?;
                Ok(Correlation {
                    parent_field: c.required("parentField", decode::compound_key)?,
                    child_field: c.required("childField", decode::compound_key)?,
                })
            })?,
            subquery: Box::new(r.required("subquery", Ast::from_json)?),
            system: r.optional("system", |v| {
                let s = decode::string(v)?;
                System::parse(&s).ok_or_else(|| DecodeError::new(format!("Invalid system \"{s}\"")))
            })?,
            hidden: r.optional_bool("hidden")?,
            aggregate: r.optional("aggregate", Aggregate::from_json)?,
        })
    }

    fn to_json(&self) -> Value {
        Builder::new()
            .set(
                "correlation",
                Builder::new()
                    .set("parentField", strings_json(&self.correlation.parent_field))
                    .set("childField", strings_json(&self.correlation.child_field))
                    .build(),
            )
            .set("subquery", self.subquery.to_json())
            .opt("system", self.system.map(System::as_str))
            .opt("hidden", self.hidden)
            .opt("aggregate", self.aggregate.as_ref().map(Aggregate::to_json))
            .build()
    }
}

impl JsonCodable for ValuePosition {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "value position")?;
        let ty = r.string("type")?;
        match ty.as_str() {
            "literal" => Ok(ValuePosition::Literal(r.required(
                "value",
                |v| match v {
                    Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => {
                        Ok(v.clone())
                    }
                    Value::Array(a) => {
                        for e in a {
                            if !matches!(e, Value::String(_) | Value::Number(_) | Value::Bool(_)) {
                                return Err(DecodeError::new("Invalid literal array element"));
                            }
                        }
                        Ok(v.clone())
                    }
                    Value::Object(_) => Err(DecodeError::new("Invalid literal value")),
                },
            )?)),
            "column" => Ok(ValuePosition::Column(r.string("name")?)),
            "json" => {
                let column = r.required("value", |v| {
                    let c = Reader::new(v, "column reference")?;
                    c.literal("type", "column")?;
                    c.string("name")
                })?;
                Ok(ValuePosition::Json {
                    column,
                    path: r.required("path", path_from_json)?,
                })
            }
            "static" => {
                let name = r.string("anchor")?;
                let anchor = Anchor::parse(&name).ok_or_else(|| {
                    DecodeError::new(format!("Invalid anchor \"{name}\"")).at("anchor")
                })?;
                let field = r.required("field", |v| match v {
                    Value::String(s) => Ok(StaticField::Single(s.clone())),
                    Value::Array(_) => Ok(StaticField::Path(decode::strings(v)?)),
                    _ => Err(DecodeError::new("Expected string or string array")),
                })?;
                Ok(ValuePosition::Static { anchor, field })
            }
            _ => Err(DecodeError::new(format!("Invalid value position type \"{ty}\"")).at("type")),
        }
    }

    fn to_json(&self) -> Value {
        match self {
            ValuePosition::Literal(v) => Builder::new()
                .set("type", "literal")
                .set("value", v.clone())
                .build(),
            ValuePosition::Column(name) => Builder::new()
                .set("type", "column")
                .set("name", name.clone())
                .build(),
            ValuePosition::Json { column, path } => Builder::new()
                .set("type", "json")
                .set(
                    "value",
                    Builder::new()
                        .set("type", "column")
                        .set("name", column.clone())
                        .build(),
                )
                .set("path", path_to_json(path))
                .build(),
            ValuePosition::Static { anchor, field } => Builder::new()
                .set("type", "static")
                .set("anchor", anchor.as_str())
                .set("field", field.to_json())
                .build(),
        }
    }
}

/// `isHomogeneousList`: every element has the JS type of the first.
fn is_homogeneous(list: &[Value]) -> bool {
    list.first()
        .is_none_or(|first| list.iter().all(|v| v.type_name() == first.type_name()))
}

impl JsonCodable for Condition {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "condition")?;
        let ty = r.string("type")?;
        match ty.as_str() {
            "simple" => {
                let name = r.string("op")?;
                let op = SimpleOperator::parse(&name).ok_or_else(|| {
                    DecodeError::new(format!("Invalid operator \"{name}\"")).at("op")
                })?;
                let left = r.required("left", ValuePosition::from_json)?;
                let right = r.required("right", ValuePosition::from_json)?;
                if matches!(right, ValuePosition::Column(_) | ValuePosition::Json { .. }) {
                    return Err(DecodeError::new(
                        "The right side of a condition can't be a column",
                    )
                    .at("right"));
                }
                // `hasJsonLeafLiteralShape` and `hasHomogeneousJsonInList`.
                if let (ValuePosition::Json { .. }, ValuePosition::Literal(value)) = (&left, &right)
                {
                    let is_in = matches!(op, SimpleOperator::In | SimpleOperator::NotIn);
                    let list = value.as_array();
                    let bad_shape = if is_in {
                        !(list.is_some() || value.is_null())
                    } else {
                        list.is_some()
                    };
                    if bad_shape {
                        return Err(DecodeError::new(
                            "expected a list for IN/NOT IN against a JSON path and a scalar for other operators",
                        ));
                    }
                    if is_in && list.is_some_and(|l| !is_homogeneous(l)) {
                        return Err(DecodeError::new(
                            "expected a list of values of one type for IN/NOT IN against a JSON path",
                        ));
                    }
                }
                Ok(Condition::Simple { op, left, right })
            }
            "and" | "or" => {
                let conditions =
                    r.required("conditions", |v| decode::array(v, Condition::from_json))?;
                Ok(if ty == "and" {
                    Condition::And(conditions)
                } else {
                    Condition::Or(conditions)
                })
            }
            "correlatedSubquery" => {
                let name = r.string("op")?;
                let op = CorrelatedSubqueryOperator::parse(&name).ok_or_else(|| {
                    DecodeError::new(format!("Invalid operator \"{name}\"")).at("op")
                })?;
                Ok(Condition::CorrelatedSubquery {
                    related: r.required("related", CorrelatedSubquery::from_json)?,
                    op,
                    flip: r.optional_bool("flip")?,
                    scalar: r.optional_bool("scalar")?,
                })
            }
            _ => Err(DecodeError::new(format!("Invalid condition type \"{ty}\"")).at("type")),
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Condition::Simple { op, left, right } => Builder::new()
                .set("type", "simple")
                .set("op", op.as_str())
                .set("left", left.to_json())
                .set("right", right.to_json())
                .build(),
            Condition::And(c) | Condition::Or(c) => Builder::new()
                .set(
                    "type",
                    if matches!(self, Condition::And(_)) {
                        "and"
                    } else {
                        "or"
                    },
                )
                .set(
                    "conditions",
                    Value::Array(c.iter().map(Condition::to_json).collect()),
                )
                .build(),
            Condition::CorrelatedSubquery {
                related,
                op,
                flip,
                scalar,
            } => Builder::new()
                .set("type", "correlatedSubquery")
                .set("related", related.to_json())
                .set("op", op.as_str())
                .opt("flip", *flip)
                .opt("scalar", *scalar)
                .build(),
        }
    }
}
