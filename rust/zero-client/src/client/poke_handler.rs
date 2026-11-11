//! Assembles the multi-part pokes of the sync protocol and merges the pokes
//! that arrive between two applies
//! (`packages/zero-client/src/client/zero-poke-handler.ts`). The engine
//! schedules the applies.

use super::replica_store::KvPatchOp;
use crate::decode::JsonCodable;
use crate::json::{self, Value, number};
use crate::keys::{
    InvalidPrimaryKeyValue, aggregate_row_key, desired_queries_key, got_queries_key,
    is_aggregate_table_name, mutation_response_key, primary_key_string,
};
use crate::protocol::{
    MutationPatchOp, PokeEndBody, PokePartBody, PokeStartBody, QueriesPatchOp, RowPatchOp,
    poke_chunk,
};
use crate::schema::{NameMapper, Schema};
use crate::strings;
use std::collections::BTreeMap;
use std::sync::Arc;

/// A merged poke, ready to apply (`PokeInternal`).
#[derive(Clone, Debug)]
pub(crate) struct MergedPoke {
    pub(crate) base_cookie: Value,
    pub(crate) cookie: Value,
    pub(crate) last_mutation_id_changes: BTreeMap<String, f64>,
    pub(crate) patch: Vec<KvPatchOp>,
}

impl MergedPoke {
    /// The poke as `JSON.stringify` shows it in errors.
    pub(crate) fn to_json(&self) -> Value {
        let mut lmids = json::Object::new();
        for (k, v) in &self.last_mutation_id_changes {
            lmids.insert(k.clone(), Value::from(*v));
        }
        let mut pull = json::Object::new();
        pull.insert("cookie", self.cookie.clone());
        pull.insert("lastMutationIDChanges", Value::Object(lmids));
        let mut o = json::Object::new();
        o.insert("baseCookie", self.base_cookie.clone());
        o.insert("pullResponse", Value::Object(pull));
        Value::Object(o)
    }
}

/// A poke that cannot be applied, as `String(error)` of the JS error.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PokeError(pub(crate) String);

impl PokeError {
    /// An `Error` with `message`.
    pub(crate) fn error(message: impl Into<String>) -> Self {
        PokeError(format!("Error: {}", message.into()))
    }
}

impl From<InvalidPrimaryKeyValue> for PokeError {
    fn from(e: InvalidPrimaryKeyValue) -> Self {
        PokeError(format!("TypeError: {e}"))
    }
}

pub(crate) struct PokeEndResult {
    pub(crate) last_mutation_id_change_for_self: Option<f64>,
}

struct Receiving {
    poke_start: PokeStartBody,
    parts: Vec<PokePartBody>,
    chunks: Vec<Vec<u8>>,
}

struct Received {
    poke_start: PokeStartBody,
    parts: Vec<PokePartBody>,
    poke_end: PokeEndBody,
}

pub(crate) struct PokeHandler {
    client_id: String,
    schema: Arc<Schema>,
    server_to_client: NameMapper,
    receiving: Option<Receiving>,
    buffer: Vec<Received>,
    pub(crate) playback_running: bool,
}

impl PokeHandler {
    pub(crate) fn new(client_id: String, schema: Arc<Schema>) -> Self {
        PokeHandler {
            server_to_client: NameMapper::server_to_client(&schema),
            client_id,
            schema,
            receiving: None,
            buffer: Vec::new(),
            playback_running: false,
        }
    }

    pub(crate) fn handle_poke_start(&mut self, poke_start: PokeStartBody) -> Result<(), String> {
        if let Some(receiving) = &self.receiving {
            return Err(format!(
                "pokeStart {} while still receiving  {} ",
                start_text(&poke_start),
                start_text(&receiving.poke_start)
            ));
        }
        self.receiving = Some(Receiving {
            poke_start,
            parts: Vec::new(),
            chunks: Vec::new(),
        });
        Ok(())
    }

    /// Returns the poke's last mutation ID change for this client.
    pub(crate) fn handle_poke_part(&mut self, part: PokePartBody) -> Result<Option<f64>, String> {
        let Some(receiving) = self
            .receiving
            .as_mut()
            .filter(|r| r.poke_start.poke_id == part.poke_id)
        else {
            return Err(format!(
                "pokePart for {}, when receiving {}",
                part.poke_id,
                self.receiving_id()
            ));
        };
        if !receiving.chunks.is_empty() {
            return Err("received both pokePart and binary poke chunks".into());
        }
        if let Some(message) = invalid_last_mutation_id(&part) {
            return Err(message);
        }
        let lmid = lmid_for(&part, &self.client_id);
        receiving.parts.push(part);
        Ok(lmid)
    }

    pub(crate) fn handle_poke_chunk(&mut self, chunk: &[u8]) -> Result<(), String> {
        let Some(receiving) = self.receiving.as_mut() else {
            return Err("received a binary poke chunk without pokeStart".into());
        };
        if !receiving.parts.is_empty() {
            return Err("received both pokePart and binary poke chunks".into());
        }
        receiving.chunks.push(chunk.to_vec());
        Ok(())
    }

    /// `Ok(None)` for a cancelled poke. When it returns a result, the poke
    /// is buffered; the engine starts the playback unless it runs.
    pub(crate) fn handle_poke_end(
        &mut self,
        poke_end: PokeEndBody,
    ) -> Result<Option<PokeEndResult>, String> {
        let matches = self
            .receiving
            .as_ref()
            .is_some_and(|r| r.poke_start.poke_id == poke_end.poke_id);
        if !matches {
            return Err(format!(
                "pokeEnd for {}, when receiving {}",
                poke_end.poke_id,
                self.receiving_id()
            ));
        }
        let receiving = self.receiving.take().expect("receiving");
        if poke_end.cancel == Some(true) {
            return Ok(None);
        }
        let mut parts = receiving.parts;
        if !receiving.chunks.is_empty() {
            parts = poke_chunk::decode(receiving.chunks.iter().map(Vec::as_slice))
                .map_err(|e| format!("TypeError: {}", e.valita_message()))?;
            for part in &parts {
                if let Some(message) = invalid_last_mutation_id(part) {
                    return Err(message);
                }
            }
        }
        let mut result = PokeEndResult {
            last_mutation_id_change_for_self: None,
        };
        for part in &parts {
            if let Some(lmid) = lmid_for(part, &self.client_id) {
                result.last_mutation_id_change_for_self = Some(lmid);
            }
        }
        self.buffer.push(Received {
            poke_start: receiving.poke_start,
            parts,
            poke_end,
        });
        Ok(Some(result))
    }

    fn receiving_id(&self) -> String {
        self.receiving
            .as_ref()
            .map_or_else(|| "undefined".to_owned(), |r| r.poke_start.poke_id.clone())
    }

    pub(crate) fn has_buffered(&self) -> bool {
        !self.buffer.is_empty()
    }

    /// Drops the poke being received and the ones buffered.
    pub(crate) fn clear(&mut self) {
        self.receiving = None;
        self.buffer.clear();
    }

    /// Merges and empties the buffer (`mergePokes`).
    pub(crate) fn take_merged(&mut self) -> Result<Option<MergedPoke>, PokeError> {
        let pokes = std::mem::take(&mut self.buffer);
        self.merge(&pokes)
    }

    fn merge(&self, pokes: &[Received]) -> Result<Option<MergedPoke>, PokeError> {
        let (Some(first), Some(last)) = (pokes.first(), pokes.last()) else {
            return Ok(None);
        };
        let mut merged = MergedPoke {
            base_cookie: first
                .poke_start
                .base_cookie
                .clone()
                .map_or(Value::Null, Value::String),
            cookie: Value::String(last.poke_end.cookie.clone()),
            last_mutation_id_changes: BTreeMap::new(),
            patch: Vec::new(),
        };
        let mut prev_poke_end: Option<&PokeEndBody> = None;
        for poke in pokes {
            if let (Some(prev), Some(base)) = (prev_poke_end, &poke.poke_start.base_cookie)
                && strings::utf16(base, &prev.cookie).is_gt()
            {
                return Err(PokeError::error(format!(
                    "unexpected cookie gap {} {}",
                    end_text(prev),
                    start_text(&poke.poke_start)
                )));
            }
            prev_poke_end = Some(&poke.poke_end);
            for part in &poke.parts {
                for (client_id, lmid) in part.last_mutation_id_changes.iter().flatten() {
                    merged
                        .last_mutation_id_changes
                        .insert(client_id.clone(), *lmid);
                }
                for (client_id, patch) in part.desired_queries_patches.iter().flatten() {
                    for op in patch {
                        merged.patch.push(query_patch_op(op, |hash| {
                            desired_queries_key(client_id, hash)
                        }));
                    }
                }
                for op in part.got_queries_patch.iter().flatten() {
                    merged.patch.push(query_patch_op(op, got_queries_key));
                }
                for op in part.rows_patch.iter().flatten() {
                    if let Some(p) = self.row_patch_op(op)? {
                        merged.patch.push(p);
                    }
                }
                for op in part.mutations_patch.iter().flatten() {
                    merged.patch.push(match op {
                        MutationPatchOp::Put(response) => KvPatchOp::Put {
                            key: mutation_response_key(&response.id.client_id, response.id.id),
                            value: response.result.to_json(),
                        },
                        MutationPatchOp::Del(id) => KvPatchOp::Del {
                            key: mutation_response_key(&id.client_id, id.id),
                        },
                    });
                }
            }
        }
        Ok(Some(merged))
    }

    fn row_patch_op(&self, op: &RowPatchOp) -> Result<Option<KvPatchOp>, PokeError> {
        if let Some(aggregate) = aggregate_row_patch_op(op)? {
            return Ok(Some(aggregate));
        }
        let mapping = |e: crate::schema::NameMappingError| PokeError::error(e.0);
        Ok(Some(match op {
            RowPatchOp::Clear => KvPatchOp::Clear,
            RowPatchOp::Put { table_name, value } => {
                let Some(table) = self.server_to_client.table_name_if_known(table_name) else {
                    return Ok(None);
                };
                let row = self
                    .server_to_client
                    .row(table_name, value)
                    .map_err(mapping)?;
                KvPatchOp::Put {
                    key: self.key(&table, &row)?,
                    value: Value::Object(row),
                }
            }
            RowPatchOp::Del { table_name, id } => {
                let Some(table) = self.server_to_client.table_name_if_known(table_name) else {
                    return Ok(None);
                };
                let id = self.server_to_client.row(table_name, id).map_err(mapping)?;
                KvPatchOp::Del {
                    key: self.key(&table, &id)?,
                }
            }
            RowPatchOp::Update {
                table_name,
                id,
                merge,
                constrain,
            } => {
                let Some(table) = self.server_to_client.table_name_if_known(table_name) else {
                    return Ok(None);
                };
                let merge = match merge {
                    Some(m) => {
                        let empty = json::Object::new();
                        let o = m.as_object().unwrap_or(&empty);
                        Some(self.server_to_client.row(table_name, o).map_err(mapping)?)
                    }
                    None => None,
                };
                let constrain = match constrain {
                    Some(c) => Some(
                        self.server_to_client
                            .columns(table_name, c)
                            .map_err(mapping)?,
                    ),
                    None => None,
                };
                let id = self.server_to_client.row(table_name, id).map_err(mapping)?;
                KvPatchOp::Update {
                    key: self.key(&table, &id)?,
                    merge,
                    constrain,
                }
            }
        }))
    }

    fn key(&self, table: &str, row: &json::Object) -> Result<String, PokeError> {
        let primary_key = &self
            .schema
            .tables
            .get(table)
            .expect("known table")
            .primary_key;
        Ok(primary_key_string(table, primary_key, row)?)
    }
}

/// `aggregateRowsPatchOp`: a synced aggregate row goes to its synthetic
/// table's `a/` key. Such tables aren't in the schema, and their columns
/// have no client names, so the row is kept as sent. `None` for another op.
fn aggregate_row_patch_op(op: &RowPatchOp) -> Result<Option<KvPatchOp>, PokeError> {
    Ok(match op {
        RowPatchOp::Put { table_name, value } if is_aggregate_table_name(table_name) => {
            Some(KvPatchOp::Put {
                key: aggregate_row_key(table_name, value)?,
                value: Value::Object(value.clone()),
            })
        }
        RowPatchOp::Del { table_name, id } if is_aggregate_table_name(table_name) => {
            Some(KvPatchOp::Del {
                key: aggregate_row_key(table_name, id)?,
            })
        }
        RowPatchOp::Update {
            table_name,
            id,
            merge,
            constrain,
        } if is_aggregate_table_name(table_name) => Some(KvPatchOp::Update {
            key: aggregate_row_key(table_name, id)?,
            merge: merge
                .as_ref()
                .map(|m| m.as_object().cloned().unwrap_or_default()),
            constrain: constrain.clone(),
        }),
        _ => None,
    })
}

fn query_patch_op(op: &QueriesPatchOp, key: impl Fn(&str) -> String) -> KvPatchOp {
    match op {
        QueriesPatchOp::Clear => KvPatchOp::Clear,
        QueriesPatchOp::Del { hash } => KvPatchOp::Del { key: key(hash) },
        QueriesPatchOp::Put { hash, .. } => KvPatchOp::Put {
            key: key(hash),
            value: Value::Null,
        },
    }
}

fn lmid_for(part: &PokePartBody, client_id: &str) -> Option<f64> {
    part.last_mutation_id_changes
        .as_ref()?
        .iter()
        .rev()
        .find(|(c, _)| c == client_id)
        .map(|(_, l)| *l)
}

/// Why `part` can't be applied: a last mutation ID that isn't a safe
/// non-negative integer.
fn invalid_last_mutation_id(part: &PokePartBody) -> Option<String> {
    for (client_id, lmid) in part.last_mutation_id_changes.iter().flatten() {
        if !(*lmid >= 0.0 && number::is_safe_integer(*lmid)) {
            return Some(format!(
                "invalid lastMutationIDChanges for {client_id}: {}",
                number::format(*lmid)
            ));
        }
    }
    None
}

/// `JSON.stringify` of a body, its keys in the order zero-cache sends them.
fn ordered_json(v: &Value, order: &[&str]) -> String {
    let Value::Object(o) = v else {
        return json::stringify(v);
    };
    let mut parts = Vec::new();
    for key in order {
        if let Some(v) = o.get(key) {
            let mut s = String::new();
            json::write_string(key, &mut s);
            parts.push(format!("{s}:{}", json::stringify(v)));
        }
    }
    format!("{{{}}}", parts.join(","))
}

fn start_text(b: &PokeStartBody) -> String {
    ordered_json(
        &b.to_json(),
        &["pokeID", "baseCookie", "schemaVersions", "timestamp"],
    )
}

fn end_text(b: &PokeEndBody) -> String {
    ordered_json(&b.to_json(), &["pokeID", "cookie", "cancel"])
}
