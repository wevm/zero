//! The wire messages: `protocol/messages.test.ts`, the `harden-*` text and
//! byte corpora of `primitives/harden-json.test.ts`, `sweep-downstream` of
//! `primitives/sweep-wire.test.ts`, and `meta.test.ts`.

use crate::case::{Case, ensure, quote, same};
use crate::{Outcome, Source, Suite};
use zero_client::decode::JsonCodable;
use zero_client::json::{self, Value};
use zero_client::protocol::{
    ClientSchema, Downstream, InitConnectionBody, PROTOCOL_VERSION, Upstream, poke_chunk,
    sec_protocols,
};
use zero_client::strings;

pub fn suites() -> Vec<Suite> {
    vec![
        Suite {
            variant: None,
            file: "protocol-downstream",
            source: Source::Checked,
            check: check::<Downstream>,
        },
        Suite {
            variant: None,
            file: "protocol-downstream-fuzz",
            source: Source::Generated,
            check: check::<Downstream>,
        },
        Suite {
            variant: None,
            file: "protocol-upstream",
            source: Source::Checked,
            check: check::<Upstream>,
        },
        Suite {
            variant: None,
            file: "protocol-upstream-fuzz",
            source: Source::Generated,
            check: check::<Upstream>,
        },
        Suite {
            variant: None,
            file: "protocol-poke-chunks",
            source: Source::Checked,
            check: poke_chunks,
        },
        Suite {
            variant: None,
            file: "protocol-poke-chunks-fuzz",
            source: Source::Generated,
            check: poke_chunks,
        },
        Suite {
            variant: None,
            file: "harden-poke-bytes",
            source: Source::Generated,
            check: poke_bytes,
        },
        Suite {
            variant: None,
            file: "harden-downstream-text",
            source: Source::Generated,
            check: downstream_text,
        },
        Suite {
            variant: None,
            file: "protocol-sec-protocols",
            source: Source::Checked,
            check: sec_protocols,
        },
        Suite {
            variant: None,
            file: "protocol-client-schema",
            source: Source::Checked,
            check: client_schema,
        },
        Suite {
            variant: None,
            file: "sweep-downstream",
            source: Source::Checked,
            check: sweep_downstream,
        },
        Suite {
            variant: None,
            file: "meta",
            source: Source::Checked,
            check: meta,
        },
    ]
}

/// A case's message and expected output, given as JSON or, for deeply
/// nested messages, as JSON text.
fn message_and_output(c: &Value) -> Result<(Value, Option<Value>), String> {
    if let Some(text) = c.get("messageText").and_then(Value::as_str) {
        let message = json::parse(text).map_err(|e| format!("messageText: {e}"))?;
        let output = match c.get("outputText").and_then(Value::as_str) {
            Some(t) => Some(json::parse(t).map_err(|e| format!("outputText: {e}"))?),
            None => None,
        };
        return Ok((message, output));
    }
    Ok((c.field("message")?.clone(), c.get("output").cloned()))
}

/// The message is accepted as JS accepts it, decodes to what JS's parse
/// returns, and what it encodes to decodes to the same message.
fn check<M: JsonCodable + PartialEq + std::fmt::Debug>(
    c: &Value,
    _: &Value,
) -> Result<Outcome, String> {
    let (message, output) = message_and_output(c)?;
    let ok = c.bool("ok")?;
    let decoded = match M::from_json(&message) {
        Ok(decoded) => decoded,
        Err(e) => {
            ensure(!ok, || format!("rejected {message}: {e}"))?;
            return Ok(Outcome::Pass);
        }
    };
    ensure(ok, || format!("accepted {message}"))?;
    if let Some(output) = output {
        same(format!("{message}"), &decoded.to_json(), &output)?;
    }
    let again = M::from_json(&decoded.to_json())
        .map_err(|e| format!("re-decoding {}: {e}", decoded.to_json()))?;
    ensure(again == decoded, || format!("round trip of {message}"))?;
    Ok(Outcome::Pass)
}

fn bytes(hex: &str) -> Result<Vec<u8>, String> {
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            hex.get(i..i + 2)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(|| format!("bad hex {hex}"))
        })
        .collect()
}

fn messages(c: &Value) -> Result<Vec<Vec<u8>>, String> {
    c.arr("messages")?
        .iter()
        .map(|m| bytes(m.as_str().ok_or("message is not a string")?))
        .collect()
}

/// Binary messages joined as bytes, then decoded.
fn decode_chunks(messages: &[Vec<u8>]) -> zero_client::decode::Result<Value> {
    let payloads = messages
        .iter()
        .map(|m| poke_chunk::payload(m))
        .collect::<Result<Vec<_>, _>>()?;
    let parts = poke_chunk::decode(payloads)?;
    Ok(Value::Array(
        parts.iter().map(JsonCodable::to_json).collect(),
    ))
}

fn poke_chunks(c: &Value, _: &Value) -> Result<Outcome, String> {
    let what = c.field("messages")?;
    match decode_chunks(&messages(c)?) {
        Ok(parts) => {
            ensure(c.bool("ok")?, || format!("accepted {what}"))?;
            same(format!("{what}"), &parts, c.field("parts")?)?;
        }
        Err(e) => ensure(!c.bool("ok")?, || format!("rejected {what}: {e}"))?,
    }
    Ok(Outcome::Pass)
}

/// Binary poke chunks edited byte by byte: accepted when `TextDecoder`,
/// `JSON.parse`, and `pokePartsSchema` accept them.
fn poke_bytes(c: &Value, _: &Value) -> Result<Outcome, String> {
    let what = c.field("messages")?;
    let result = decode_chunks(&messages(c)?);
    same(format!("accepts {what}"), result.is_ok(), c.bool("ok")?)?;
    Ok(Outcome::Pass)
}

/// Downstream messages edited as text: accepted when `JSON.parse` and
/// `downstreamSchema` accept them.
fn downstream_text(c: &Value, _: &Value) -> Result<Outcome, String> {
    let text = c.str("text")?;
    let result = Downstream::from_text(text);
    match (result, c.bool("ok")?) {
        (Ok(_), true) | (Err(_), false) => Ok(Outcome::Pass),
        (Err(e), true) if e.message.contains(json::TOO_DEEP) => {
            Ok(Outcome::Exception(super::json::TOO_DEEP))
        }
        (Ok(_), false) => Err(format!("accepted {}", quote(text))),
        (Err(e), true) => Err(format!("rejected {}: {e}", quote(text))),
    }
}

fn sec_protocols(c: &Value, _: &Value) -> Result<Outcome, String> {
    let encoded = c.str("encoded")?;
    match sec_protocols::decode(encoded) {
        Ok(decoded) => {
            ensure(c.bool("ok")?, || format!("accepted {encoded}"))?;
            same(encoded, &decoded, c.field("decoded")?)?;
        }
        Err(e) => {
            ensure(!c.bool("ok")?, || format!("rejected {encoded}: {e}"))?;
            return Ok(Outcome::Pass);
        }
    }
    let message = c.get("initConnectionMessage");
    let auth_token = c.get("authToken").and_then(Value::as_str);
    if message.is_none() && auth_token.is_none() {
        return Ok(Outcome::Pass);
    }
    // Rust's encoding differs in key order, but carries the same values.
    let body = match message {
        Some(m) => {
            let body = Upstream::from_json(m).map_err(|e| format!("initConnection: {e}"))?;
            let Upstream::InitConnection(body) = body else {
                return Err(format!("{m} is not initConnection"));
            };
            Some(body)
        }
        None => None,
    };
    let ours = sec_protocols::encode(body.as_ref().map(|b: &InitConnectionBody| b), auth_token);
    let decoded = sec_protocols::decode(&ours).map_err(|e| format!("decode {ours}: {e}"))?;
    same(
        format!("re-encoded {encoded}"),
        &decoded,
        c.field("decoded")?,
    )?;
    Ok(Outcome::Pass)
}

fn client_schema(c: &Value, _: &Value) -> Result<Outcome, String> {
    let json = c.field("schema")?;
    let schema = ClientSchema::from_json(json).map_err(|e| format!("rejected {json}: {e}"))?;
    same("encode", &schema.to_json(), json)?;
    let normalized = json::stringify(&schema.normalized().to_json());
    same(
        format!("normalize {json}"),
        normalized.as_str(),
        c.str("normalized")?,
    )?;
    Ok(Outcome::Pass)
}

/// `JSON.stringify` with object keys sorted by UTF-16 code unit, as the
/// generator's `sortedStringify` writes it.
fn sorted_stringify(v: &Value, out: &mut String) {
    match v {
        Value::Array(a) => {
            out.push('[');
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                sorted_stringify(e, out);
            }
            out.push(']');
        }
        Value::Object(o) => {
            let mut entries: Vec<_> = o.iter().collect();
            entries.sort_by(|a, b| strings::utf16(a.0, b.0));
            out.push('{');
            for (i, (k, v)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json::write_string(k, out);
                out.push(':');
                sorted_stringify(v, out);
            }
            out.push('}');
        }
        v => json::write(v, out),
    }
}

/// Downstream messages with keys JS keeps apart: rows, primary keys, merges,
/// client IDs, and `__proto__`. Compared with keys sorted: Rust re-encodes a
/// message from its fields, in an order of its own.
fn sweep_downstream(c: &Value, _: &Value) -> Result<Outcome, String> {
    let text = c.str("text")?;
    let message = Downstream::from_text(text).map_err(|e| format!("rejected {text}: {e}"))?;
    let mut got = String::new();
    sorted_stringify(&message.to_json(), &mut got);
    same(text, got.as_str(), c.str("output")?)?;
    Ok(Outcome::Pass)
}

/// The server keeps supporting every protocol from its historical floor, so
/// the Rust client may lag the JS one but must stay inside that window.
fn meta(c: &Value, _: &Value) -> Result<Outcome, String> {
    let version = f64::from(PROTOCOL_VERSION);
    let (min, max) = (
        c.num("minServerSupportedSyncProtocol")?,
        c.num("protocolVersion")?,
    );
    ensure(version >= min && version <= max, || {
        format!("protocol {version} is outside the server's {min} to {max}")
    })?;
    Ok(Outcome::Pass)
}
