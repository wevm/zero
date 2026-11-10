//! A Zero schema's tables and columns (`packages/zero-types/src/schema.ts`),
//! and the names they map to on the server
//! (`packages/zero-types/src/name-mapper.ts`).

use crate::ast::*;
use crate::decode::{self, DecodeError, Reader, Result};
use crate::json::{Object, Value};
use crate::map::OrderedMap;
use std::collections::{HashMap, HashSet};
use std::fmt;

crate::ast::string_enum!(
    /// The type of a column value (`ValueType` in `schema-value.ts`).
    ValueType { String = "string", Number = "number", Boolean = "boolean", Null = "null", Json = "json" }
);

/// A column (`SchemaValue`).
#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub type_: ValueType,
    pub optional: bool,
    pub server_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    pub name: String,
    pub server_name: Option<String>,
    /// In declaration order.
    pub columns: OrderedMap<Column>,
    pub primary_key: Vec<String>,
}

crate::ast::string_enum!(
    /// How many rows a relationship hop leads to.
    Cardinality { One = "one", Many = "many" }
);

/// One hop of a relationship: from `source_field` of the table to
/// `dest_field` of `dest_schema`.
#[derive(Clone, Debug, PartialEq)]
pub struct RelationshipHop {
    pub source_field: Vec<String>,
    pub dest_field: Vec<String>,
    pub dest_schema: String,
    pub cardinality: Cardinality,
}

/// A relationship: one hop, or two through a junction table.
pub type Relationship = Vec<RelationshipHop>;

/// The tables of a schema and their relationships, in declaration order.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Schema {
    pub tables: OrderedMap<Table>,
    /// By table, then by name.
    pub relationships: OrderedMap<OrderedMap<Relationship>>,
}

impl Schema {
    /// A schema in the JSON form JS's schema object has (`tables`, each with
    /// `name`, `serverName`, `columns`, and `primaryKey`, and optionally
    /// `relationships`).
    pub fn from_json(v: &Value) -> Result<Schema> {
        let r = Reader::new(v, "schema")?;
        let tables = r.required("tables", |v| {
            decode::record(v, |t| {
                let t = Reader::new(t, "table")?;
                Ok(Table {
                    name: t.string("name")?,
                    server_name: t.optional_string("serverName")?,
                    columns: t
                        .required("columns", |v| {
                            decode::record(v, |c| {
                                let c = Reader::new(c, "column")?;
                                let ty = c.string("type")?;
                                Ok(Column {
                                    type_: ValueType::parse(&ty).ok_or_else(|| {
                                        DecodeError::new(format!("Invalid type \"{ty}\""))
                                            .at("type")
                                    })?,
                                    optional: c.optional_bool("optional")?.unwrap_or(false),
                                    server_name: c.optional_string("serverName")?,
                                })
                            })
                        })?
                        .into_iter()
                        .collect(),
                    primary_key: t.required("primaryKey", decode::compound_key)?,
                })
            })
        })?;
        let relationships = r
            .optional("relationships", |v| {
                decode::record(v, |rels| {
                    Ok(
                        decode::record(rels, |hops| decode::array(hops, hop_from_json))?
                            .into_iter()
                            .collect::<OrderedMap<_>>(),
                    )
                })
            })?
            .unwrap_or_default();
        Ok(Schema {
            tables: tables.into_iter().collect(),
            relationships: relationships.into_iter().collect(),
        })
    }

    /// The schema zero-cache checks the client's against, with server names
    /// (`clientSchemaFrom` in `packages/zero-schema/src/builder/schema-builder.ts`).
    pub fn client_schema(&self) -> crate::protocol::ClientSchema {
        use crate::protocol::{ClientSchema, ClientTable};
        let tables = self
            .tables
            .iter()
            .map(|(name, t)| {
                let server = |c: &str| {
                    t.columns
                        .get(c)
                        .and_then(|c| c.server_name.clone())
                        .unwrap_or_else(|| c.to_owned())
                };
                let table = ClientTable {
                    columns: t
                        .columns
                        .iter()
                        .map(|(c, col)| (server(c), col.type_))
                        .collect(),
                    primary_key: t.primary_key.iter().map(|k| server(k)).collect(),
                };
                (t.server_name.clone().unwrap_or_else(|| name.clone()), table)
            })
            .collect();
        ClientSchema { tables }.normalized()
    }

    /// The relationship `name` of `table`.
    pub fn relationship(&self, table: &str, name: &str) -> Option<&Relationship> {
        self.relationships.get(table)?.get(name)
    }
}

fn hop_from_json(v: &Value) -> Result<RelationshipHop> {
    let h = Reader::new(v, "relationship")?;
    let cardinality = h.string("cardinality")?;
    Ok(RelationshipHop {
        source_field: h.required("sourceField", decode::strings)?,
        dest_field: h.required("destField", decode::strings)?,
        dest_schema: h.string("destSchema")?,
        cardinality: Cardinality::parse(&cardinality).ok_or_else(|| {
            DecodeError::new(format!("Invalid cardinality \"{cardinality}\"")).at("cardinality")
        })?,
    })
}

/// An unknown table or column met while mapping names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameMappingError(pub String);

impl fmt::Display for NameMappingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NameMappingError {}

type MapResult<T> = std::result::Result<T, NameMappingError>;

struct Destination {
    table_name: String,
    columns: HashMap<String, String>,
    all_columns_same: bool,
    /// The source names of the table's `json` columns.
    json_columns: HashSet<String>,
}

/// Maps table and column names between the client and the server
/// (`NameMapper`).
pub struct NameMapper {
    tables: HashMap<String, Destination>,
}

impl NameMapper {
    /// `clientToServer`.
    pub fn client_to_server(schema: &Schema) -> Self {
        Self::new(schema, true)
    }

    /// `serverToClient`.
    pub fn server_to_client(schema: &Schema) -> Self {
        Self::new(schema, false)
    }

    fn new(schema: &Schema, from_client: bool) -> Self {
        let mut tables = HashMap::new();
        for (name, table) in &schema.tables {
            let mut all_columns_same = true;
            let mut columns = HashMap::new();
            let mut json_columns = HashSet::new();
            for (column, c) in &table.columns {
                let server = c.server_name.as_deref().unwrap_or(column);
                if server != column {
                    all_columns_same = false;
                }
                let (src, dst) = if from_client {
                    (column.as_str(), server)
                } else {
                    (server, column.as_str())
                };
                columns.insert(src.to_owned(), dst.to_owned());
                if c.type_ == ValueType::Json {
                    json_columns.insert(src.to_owned());
                }
            }
            let server_name = table.server_name.as_deref().unwrap_or(name);
            let (src, dst) = if from_client {
                (name.as_str(), server_name)
            } else {
                (server_name, name.as_str())
            };
            tables.insert(
                src.to_owned(),
                Destination {
                    table_name: dst.to_owned(),
                    columns,
                    all_columns_same,
                    json_columns,
                },
            );
        }
        NameMapper { tables }
    }

    fn table(&self, src: &str) -> MapResult<&Destination> {
        self.tables
            .get(src)
            .ok_or_else(|| NameMappingError(format!("unknown table \"{src}\"")))
    }

    pub fn table_name(&self, src: &str) -> MapResult<String> {
        Ok(self.table(src)?.table_name.clone())
    }

    pub fn column_name(&self, table: &str, src: &str) -> MapResult<String> {
        self.table(table)?.columns.get(src).cloned().ok_or_else(|| {
            NameMappingError(format!("unknown column \"{src}\" of \"{table}\" table"))
        })
    }

    /// `jsonColumnName`: the column a JSON path reads, which must be a `json`
    /// column. An unknown column fails the same way as a known one that
    /// isn't json.
    pub fn json_column_name(&self, table: &str, src: &str) -> MapResult<String> {
        if !self.table(table)?.json_columns.contains(src) {
            return Err(NameMappingError(format!(
                "column \"{src}\" of \"{table}\" table is not a json column"
            )));
        }
        self.column_name(table, src)
    }

    fn column_names(&self, table: &str, columns: &[String]) -> MapResult<Vec<String>> {
        columns.iter().map(|c| self.column_name(table, c)).collect()
    }

    /// Renames the known columns of `row`; others keep their names.
    pub fn row(&self, table: &str, row: &Object) -> MapResult<Object> {
        let dest = self.table(table)?;
        if dest.all_columns_same {
            return Ok(row.clone());
        }
        Ok(row
            .iter()
            .map(|(c, v)| (dest.columns.get(c).unwrap_or(c).clone(), v.clone()))
            .collect())
    }

    /// `mapAST`: the AST with its table and column names mapped.
    pub fn map(&self, ast: &Ast) -> MapResult<Ast> {
        let mut out = ast.clone();
        out.table = self.table_name(&ast.table)?;
        out.where_ = ast
            .where_
            .as_ref()
            .map(|w| self.map_condition(w, &ast.table))
            .transpose()?;
        if let Some(related) = &ast.related {
            let mut mapped = Vec::with_capacity(related.len());
            for r in related {
                let mut r2 = r.clone();
                r2.correlation =
                    self.map_correlation(&r.correlation, &ast.table, &r.subquery.table)?;
                r2.subquery = Box::new(self.map(&r.subquery)?);
                // A junction aggregate's field is a column of the destination.
                let field_table = r
                    .subquery
                    .related
                    .as_ref()
                    .and_then(|rel| rel.first())
                    .map_or(&r.subquery.table, |j| &j.subquery.table);
                r2.aggregate = r
                    .aggregate
                    .as_ref()
                    .map(|a| self.map_aggregate(a, field_table))
                    .transpose()?;
                mapped.push(r2);
            }
            out.related = Some(mapped);
        }
        if let Some(start) = &ast.start {
            let mut row = Object::new();
            for (column, value) in &start.row {
                row.insert(self.column_name(&ast.table, column)?, value.clone());
            }
            out.start = Some(Bound {
                row,
                exclusive: start.exclusive,
            });
        }
        if let Some(order_by) = &ast.order_by {
            out.order_by = Some(
                order_by
                    .iter()
                    .map(|p| {
                        Ok(OrderPart {
                            field: self.column_name(&ast.table, &p.field)?,
                            direction: p.direction,
                        })
                    })
                    .collect::<MapResult<_>>()?,
            );
        }
        out.aggregate = ast
            .aggregate
            .as_ref()
            .map(|a| self.map_aggregate(a, &ast.table))
            .transpose()?;
        out.group_by = ast
            .group_by
            .as_ref()
            .map(|g| self.column_names(&ast.table, g))
            .transpose()?;
        Ok(out)
    }

    /// `transformAggregate`: the field maps, and must be a `json` column when
    /// there is a path; the path is data.
    fn map_aggregate(&self, a: &Aggregate, table: &str) -> MapResult<Aggregate> {
        let mut out = Aggregate::new(a.fn_);
        if let Some(distinct) = &a.distinct {
            out.distinct = Some(self.column_names(table, distinct)?);
        } else if let Some(field) = &a.field {
            match &a.path {
                None => out.field = Some(self.column_name(table, field)?),
                Some(path) => {
                    out.field = Some(self.json_column_name(table, field)?);
                    out.path = Some(path.clone());
                }
            }
        }
        Ok(out)
    }

    /// `mapCondition`. Only the left side of a comparison names a column.
    pub fn map_condition(&self, condition: &Condition, table: &str) -> MapResult<Condition> {
        Ok(match condition {
            Condition::Simple { op, left, right } => {
                let left = match left {
                    ValuePosition::Column(name) => {
                        ValuePosition::Column(self.column_name(table, name)?)
                    }
                    ValuePosition::Json { column, path } => ValuePosition::Json {
                        column: self.json_column_name(table, column)?,
                        path: path.clone(),
                    },
                    _ => return Ok(condition.clone()),
                };
                Condition::Simple {
                    op: *op,
                    left,
                    right: right.clone(),
                }
            }
            Condition::CorrelatedSubquery {
                related,
                op,
                flip,
                scalar,
            } => {
                let mut related = related.clone();
                related.correlation =
                    self.map_correlation(&related.correlation, table, &related.subquery.table)?;
                related.subquery = Box::new(self.map(&related.subquery)?);
                Condition::CorrelatedSubquery {
                    related,
                    op: *op,
                    flip: *flip,
                    scalar: *scalar,
                }
            }
            Condition::And(c) => Condition::And(
                c.iter()
                    .map(|c| self.map_condition(c, table))
                    .collect::<MapResult<_>>()?,
            ),
            Condition::Or(c) => Condition::Or(
                c.iter()
                    .map(|c| self.map_condition(c, table))
                    .collect::<MapResult<_>>()?,
            ),
        })
    }

    fn map_correlation(
        &self,
        c: &Correlation,
        parent: &str,
        child: &str,
    ) -> MapResult<Correlation> {
        Ok(Correlation {
            parent_field: self.column_names(parent, &c.parent_field)?,
            child_field: self.column_names(child, &c.child_field)?,
        })
    }
}
