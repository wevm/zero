//! The schema a client was built with, sent so zero-cache can check it
//! against the replica (`clientSchemaSchema` in
//! `packages/zero-protocol/src/client-schema.ts`).

use super::*;
use crate::schema::ValueType;
use crate::strings;

/// `tableSchemaSchema`: each column's `{type}`, and the primary key.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientTable {
    /// In the order they were sent.
    pub columns: Vec<(String, ValueType)>,
    pub primary_key: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct ClientSchema {
    /// In the order they were sent.
    pub tables: Vec<(String, ClientTable)>,
}

impl JsonCodable for ClientSchema {
    fn from_json(v: &Value) -> Result<Self> {
        let names: Vec<&str> = ValueType::ALL.iter().map(|t| t.as_str()).collect();
        let r = Reader::new(v, "client schema")?;
        let tables = r.required("tables", |v| {
            decode::record(v, |t| {
                let t = Reader::new(t, "table schema")?;
                Ok(ClientTable {
                    columns: t.required("columns", |v| {
                        decode::record(v, |c| {
                            let c = Reader::new(c, "column schema")?;
                            c.required("type", |v| {
                                let name = literal_union(v, &names)?;
                                Ok(ValueType::parse(name).expect("listed"))
                            })
                        })
                    })?,
                    primary_key: t.required("primaryKey", decode::strings)?,
                })
            })
        })?;
        Ok(ClientSchema { tables })
    }

    fn to_json(&self) -> Value {
        let tables: Object = self
            .tables
            .iter()
            .map(|(name, t)| {
                let columns: Object = t
                    .columns
                    .iter()
                    .map(|(c, ty)| (c.clone(), Builder::new().set("type", ty.as_str()).build()))
                    .collect();
                let table = Builder::new()
                    .set("columns", columns)
                    .set("primaryKey", decode::strings_json(&t.primary_key))
                    .build();
                (name.clone(), table)
            })
            .collect();
        Builder::new().set("tables", tables).build()
    }
}

impl ClientSchema {
    /// `normalizeClientSchema`: tables, columns, and each primary key sorted
    /// by UTF-16 code unit, as JS `sort()` and `<` sort them.
    pub fn normalized(&self) -> ClientSchema {
        let mut tables: Vec<(String, ClientTable)> = self
            .tables
            .iter()
            .map(|(name, t)| {
                let mut columns = t.columns.clone();
                columns.sort_by(|a, b| strings::utf16(&a.0, &b.0));
                let mut primary_key = t.primary_key.clone();
                primary_key.sort_by(|a, b| strings::utf16(a, b));
                (
                    name.clone(),
                    ClientTable {
                        columns,
                        primary_key,
                    },
                )
            })
            .collect();
        tables.sort_by(|a, b| strings::utf16(&a.0, &b.0));
        ClientSchema { tables }
    }
}
