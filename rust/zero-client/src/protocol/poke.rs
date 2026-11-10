//! Pokes (`packages/zero-protocol/src/poke.ts`), row patches
//! (`row-patch.ts`), and binary poke chunks.

use super::*;

/// The range of schema versions the server supports.
#[derive(Clone, Debug, PartialEq)]
pub struct SchemaVersions {
    pub min_supported_version: f64,
    pub max_supported_version: f64,
}

/// Starts a poke (`pokeStartBodySchema`).
#[derive(Clone, Debug, PartialEq)]
pub struct PokeStartBody {
    pub poke_id: String,
    /// The cookie the poke applies to; `None` for a client that has never
    /// pulled.
    pub base_cookie: Option<Version>,
    /// Always set when the poke has a `rowsPatch`.
    pub schema_versions: Option<SchemaVersions>,
    pub timestamp: Option<f64>,
}

impl JsonCodable for PokeStartBody {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "pokeStart body")?;
        Ok(PokeStartBody {
            poke_id: r.string("pokeID")?,
            base_cookie: r.required("baseCookie", nullable_version)?,
            schema_versions: r.optional("schemaVersions", |v| {
                let s = Reader::new(v, "schema versions")?;
                Ok(SchemaVersions {
                    min_supported_version: s.number("minSupportedVersion")?,
                    max_supported_version: s.number("maxSupportedVersion")?,
                })
            })?,
            timestamp: r.optional_number("timestamp")?,
        })
    }

    fn to_json(&self) -> Value {
        Builder::new()
            .set("pokeID", self.poke_id.clone())
            .set("baseCookie", nullable_version_json(&self.base_cookie))
            .opt(
                "schemaVersions",
                self.schema_versions.as_ref().map(|s| {
                    Builder::new()
                        .set("minSupportedVersion", s.min_supported_version)
                        .set("maxSupportedVersion", s.max_supported_version)
                        .build()
                }),
            )
            .opt("timestamp", self.timestamp)
            .build()
    }
}

/// A change to the synced rows (`rowPatchOpSchema`).
#[derive(Clone, Debug, PartialEq)]
pub enum RowPatchOp {
    /// Inserts or replaces the row.
    Put {
        table_name: String,
        value: RowRecord,
    },
    /// Merges `merge` into the row with primary key `id`. `merge` is a
    /// `jsonObjectSchema`, which admits an array as well as an object; it is
    /// kept as sent. `constrain` limits the columns the row keeps.
    Update {
        table_name: String,
        id: PrimaryKeyValueRecord,
        merge: Option<Value>,
        constrain: Option<Vec<String>>,
    },
    /// Deletes the row with primary key `id`.
    Del {
        table_name: String,
        id: PrimaryKeyValueRecord,
    },
    /// Deletes every row.
    Clear,
}

impl JsonCodable for RowPatchOp {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "row patch op")?;
        match r.string("op")?.as_str() {
            "put" => Ok(RowPatchOp::Put {
                table_name: r.string("tableName")?,
                value: r.required("value", decode::object)?,
            }),
            "update" => Ok(RowPatchOp::Update {
                table_name: r.string("tableName")?,
                id: r.required("id", primary_key_value_record)?,
                merge: r.optional("merge", json_object)?,
                constrain: r.optional("constrain", decode::strings)?,
            }),
            "del" => Ok(RowPatchOp::Del {
                table_name: r.string("tableName")?,
                id: r.required("id", primary_key_value_record)?,
            }),
            "clear" => Ok(RowPatchOp::Clear),
            op => Err(DecodeError::new(format!("Invalid row patch op \"{op}\"")).at("op")),
        }
    }

    fn to_json(&self) -> Value {
        match self {
            RowPatchOp::Put { table_name, value } => Builder::new()
                .set("op", "put")
                .set("tableName", table_name.clone())
                .set("value", value.clone()),
            RowPatchOp::Update {
                table_name,
                id,
                merge,
                constrain,
            } => Builder::new()
                .set("op", "update")
                .set("tableName", table_name.clone())
                .set("id", id.clone())
                .opt("merge", merge.clone())
                .opt("constrain", constrain.as_deref().map(decode::strings_json)),
            RowPatchOp::Del { table_name, id } => Builder::new()
                .set("op", "del")
                .set("tableName", table_name.clone())
                .set("id", id.clone()),
            RowPatchOp::Clear => Builder::new().set("op", "clear"),
        }
        .build()
    }
}

/// One part of a poke (`pokePartBodySchema`).
#[derive(Clone, Debug, PartialEq)]
pub struct PokePartBody {
    pub poke_id: String,
    /// The new last mutation ID of each client that changed.
    pub last_mutation_id_changes: Option<Vec<(String, f64)>>,
    /// Changes to each client's desired queries, in the order the server
    /// sent the clients: a `clear` for one client and a `put` for another
    /// must apply in that order.
    pub desired_queries_patches: Option<Vec<(String, QueriesPatch)>>,
    /// Changes to the queries whose rows `rows_patch` syncs.
    pub got_queries_patch: Option<QueriesPatch>,
    pub rows_patch: Option<Vec<RowPatchOp>>,
    pub mutations_patch: Option<Vec<MutationPatchOp>>,
}

impl JsonCodable for PokePartBody {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "pokePart body")?;
        Ok(PokePartBody {
            last_mutation_id_changes: r.optional("lastMutationIDChanges", number_record)?,
            desired_queries_patches: r.optional("desiredQueriesPatches", |v| {
                decode::record(v, queries_patch)
            })?,
            got_queries_patch: r.optional("gotQueriesPatch", queries_patch)?,
            rows_patch: r.optional("rowsPatch", |v| decode::array(v, RowPatchOp::from_json))?,
            mutations_patch: r.optional("mutationsPatch", |v| {
                decode::array(v, MutationPatchOp::from_json)
            })?,
            poke_id: r.string("pokeID")?,
        })
    }

    fn to_json(&self) -> Value {
        Builder::new()
            .opt(
                "lastMutationIDChanges",
                self.last_mutation_id_changes
                    .as_deref()
                    .map(number_record_json),
            )
            .opt(
                "desiredQueriesPatches",
                self.desired_queries_patches.as_ref().map(|p| {
                    Value::Object(p.iter().map(|(k, q)| (k.clone(), array_json(q))).collect())
                }),
            )
            .opt(
                "gotQueriesPatch",
                self.got_queries_patch.as_deref().map(array_json),
            )
            .opt("rowsPatch", self.rows_patch.as_deref().map(array_json))
            .opt(
                "mutationsPatch",
                self.mutations_patch.as_deref().map(array_json),
            )
            .set("pokeID", self.poke_id.clone())
            .build()
    }
}

/// Ends a poke (`pokeEndBodySchema`).
#[derive(Clone, Debug, PartialEq)]
pub struct PokeEndBody {
    pub poke_id: String,
    /// The cookie after the poke. Ignore it, and it may be empty, if
    /// `cancel` is set.
    pub cookie: Version,
    /// Discard the poke without applying it.
    pub cancel: Option<bool>,
}

impl JsonCodable for PokeEndBody {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "pokeEnd body")?;
        Ok(PokeEndBody {
            poke_id: r.string("pokeID")?,
            cookie: r.string("cookie")?,
            cancel: r.optional_bool("cancel")?,
        })
    }

    fn to_json(&self) -> Value {
        Builder::new()
            .set("pokeID", self.poke_id.clone())
            .set("cookie", self.cookie.clone())
            .opt("cancel", self.cancel)
            .build()
    }
}

/// The binary poke chunks of protocol 52 and later. Between `pokeStart` and
/// `pokeEnd` zero-cache may send a poke's parts as binary messages instead of
/// `pokePart` messages: each is a [`MESSAGE_TYPE`](poke_chunk::MESSAGE_TYPE)
/// byte and then the next bytes of the UTF-8 JSON of a `PokePartBody[]`. A
/// chunk can end inside a UTF-8 sequence.
pub mod poke_chunk {
    use super::*;

    /// `POKE_CHUNK_MESSAGE_TYPE`.
    pub const MESSAGE_TYPE: u8 = 0x00;
    /// `POKE_CHUNK_PROTOCOL_VERSION`.
    pub const PROTOCOL_VERSION: u32 = 52;

    /// The payload of a binary message. Fails for a message of another
    /// type, which the client treats as an invalid message.
    pub fn payload(message: &[u8]) -> Result<&[u8]> {
        match message.split_first() {
            None => Err(DecodeError::new("Unknown binary message type: undefined")),
            Some((&MESSAGE_TYPE, rest)) => Ok(rest),
            Some((t, _)) => Err(DecodeError::new(format!(
                "Unknown binary message type: {t}"
            ))),
        }
    }

    /// Decodes the payloads of a poke's chunks, in order, into its parts
    /// (`decodePokeChunks`): strict UTF-8, a leading BOM dropped as
    /// `TextDecoder` does, then `pokePartsSchema`.
    pub fn decode<'a>(chunks: impl IntoIterator<Item = &'a [u8]>) -> Result<Vec<PokePartBody>> {
        let bytes: Vec<u8> = chunks.into_iter().flatten().copied().collect();
        let payload = bytes
            .strip_prefix(&[0xEF, 0xBB, 0xBF][..])
            .unwrap_or(&bytes);
        let text = std::str::from_utf8(payload)
            .map_err(|_| DecodeError::new("The encoded data was not valid for encoding utf-8"))?;
        decode::array(&parse_message(text)?, PokePartBody::from_json)
    }
}
