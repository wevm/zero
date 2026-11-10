//! The shape a query's view materializes as (`packages/zero-types/src/format.ts`).

use crate::ast::Aggregate;
use crate::decode::{self, JsonCodable, Reader, Result};
use crate::json::{Object, Value};
use crate::strings;

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Format {
    pub singular: bool,
    /// By name, in UTF-16 code unit order.
    pub relationships: Vec<(String, Format)>,
    /// Set when the query or relationship is an aggregate: it materializes as
    /// the aggregate's value rather than rows. Implies `singular`.
    pub aggregate: Option<Aggregate>,
}

impl Format {
    /// Sets the format of the relationship `name`, keeping the
    /// relationships in order.
    pub fn set_relationship(&mut self, name: &str, format: Format) {
        match self
            .relationships
            .binary_search_by(|(k, _)| strings::utf16(k, name))
        {
            Ok(i) => self.relationships[i].1 = format,
            Err(i) => self.relationships.insert(i, (name.to_owned(), format)),
        }
    }
}

impl JsonCodable for Format {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "object")?;
        let mut relationships = r
            .optional("relationships", |v| decode::record(v, Format::from_json))?
            .unwrap_or_default();
        relationships.sort_by(|a, b| strings::utf16(&a.0, &b.0));
        Ok(Format {
            singular: r.optional_bool("singular")?.unwrap_or(false),
            relationships,
            aggregate: r.optional("aggregate", Aggregate::from_json)?,
        })
    }

    fn to_json(&self) -> Value {
        let rels: Object = self
            .relationships
            .iter()
            .map(|(k, f)| (k.clone(), f.to_json()))
            .collect();
        let mut o = Object::new();
        o.insert("singular", Value::Bool(self.singular));
        o.insert("relationships", Value::Object(rels));
        if let Some(a) = &self.aggregate {
            o.insert("aggregate", a.to_json());
        }
        Value::Object(o)
    }
}
