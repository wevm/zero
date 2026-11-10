//! `connected`, `initConnection`, and the `Sec-WebSocket-Protocol` header
//! that carries it (`packages/zero-protocol/src/connect.ts`).

use super::*;

/// The `connected` message zero-cache sends once a connection is set up.
#[derive(Clone, Debug, PartialEq)]
pub struct ConnectedBody {
    pub wsid: String,
    pub timestamp: Option<f64>,
}

impl JsonCodable for ConnectedBody {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "connected body")?;
        Ok(ConnectedBody {
            wsid: r.string("wsid")?,
            timestamp: r.optional_number("timestamp")?,
        })
    }

    fn to_json(&self) -> Value {
        Builder::new()
            .set("wsid", self.wsid.clone())
            .opt("timestamp", self.timestamp)
            .build()
    }
}

/// The first message a client sends, usually in the `Sec-WebSocket-Protocol`
/// header (`initConnectionBodySchema`).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct InitConnectionBody {
    pub desired_queries_patch: UpQueriesPatch,
    /// Sent only when the client has no server snapshot yet: the schema can
    /// be large, and zero-cache keeps it for the client group.
    pub client_schema: Option<ClientSchema>,
    pub deleted: Option<DeleteClientsBody>,
    /// The push endpoint of the API server.
    pub user_push_url: Option<String>,
    pub user_push_headers: Option<Vec<(String, String)>>,
    /// The query endpoint of the API server.
    pub user_query_url: Option<String>,
    pub user_query_headers: Option<Vec<(String, String)>>,
    /// The client IDs running in the client group, so zero-cache can
    /// inactivate the queries of the others.
    pub active_clients: Option<Vec<String>>,
    /// A W3C traceparent header for distributed tracing.
    pub traceparent: Option<String>,
}

impl JsonCodable for InitConnectionBody {
    fn from_json(v: &Value) -> Result<Self> {
        let r = Reader::new(v, "initConnection body")?;
        Ok(InitConnectionBody {
            desired_queries_patch: r.required("desiredQueriesPatch", up_queries_patch)?,
            client_schema: r.optional("clientSchema", ClientSchema::from_json)?,
            deleted: r.optional("deleted", DeleteClientsBody::from_json)?,
            user_push_url: r.optional_string("userPushURL")?,
            user_push_headers: r.optional("userPushHeaders", string_record)?,
            user_query_url: r.optional_string("userQueryURL")?,
            user_query_headers: r.optional("userQueryHeaders", string_record)?,
            active_clients: r.optional("activeClients", decode::strings)?,
            traceparent: r.optional_string("traceparent")?,
        })
    }

    fn to_json(&self) -> Value {
        Builder::new()
            .set(
                "desiredQueriesPatch",
                array_json(&self.desired_queries_patch),
            )
            .opt(
                "clientSchema",
                self.client_schema.as_ref().map(ClientSchema::to_json),
            )
            .opt(
                "deleted",
                self.deleted.as_ref().map(DeleteClientsBody::to_json),
            )
            .opt("userPushURL", self.user_push_url.clone())
            .opt(
                "userPushHeaders",
                self.user_push_headers.as_deref().map(string_record_json),
            )
            .opt("userQueryURL", self.user_query_url.clone())
            .opt(
                "userQueryHeaders",
                self.user_query_headers.as_deref().map(string_record_json),
            )
            .opt(
                "activeClients",
                self.active_clients.as_deref().map(decode::strings_json),
            )
            .opt("traceparent", self.traceparent.clone())
            .build()
    }
}

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// The `Sec-WebSocket-Protocol` value that carries the `initConnection`
/// message and the auth token (`encodeSecProtocols` and
/// `decodeSecProtocols`): the UTF-8 of
/// `JSON.stringify({initConnectionMessage, authToken})`, base64 encoded,
/// then URI encoded.
pub mod sec_protocols {
    use super::*;

    pub fn encode(
        init_connection: Option<&InitConnectionBody>,
        auth_token: Option<&str>,
    ) -> String {
        let value = Builder::new()
            .opt(
                "initConnectionMessage",
                init_connection.map(|b| message("initConnection", b.to_json())),
            )
            .opt("authToken", auth_token)
            .build();
        let base64 = base64_encode(json::stringify(&value).as_bytes());
        // `encodeURIComponent` of base64 only escapes `+`, `/`, and `=`.
        base64
            .replace('+', "%2B")
            .replace('/', "%2F")
            .replace('=', "%3D")
    }

    /// Decodes a header value into the JSON it carries, as `JSON.parse`
    /// returns it: `decodeSecProtocols` does not validate it.
    pub fn decode(sec_protocol: &str) -> Result<Value> {
        let base64 = uri_component_decode(sec_protocol)?;
        let bytes = forgiving_base64_decode(&base64)
            .ok_or_else(|| DecodeError::new("The string to be decoded is not correctly encoded"))?;
        // `new TextDecoder().decode`: replaces invalid UTF-8 and drops a BOM.
        let bytes = bytes.strip_prefix(UTF8_BOM).unwrap_or(&bytes);
        parse_message(&String::from_utf8_lossy(bytes))
    }
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Base64 as `btoa` writes it.
fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The WHATWG "forgiving-base64 decode" that `atob` uses.
fn forgiving_base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut data = Vec::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\t' | '\n' | '\u{c}' | '\r' | ' ' => {}
            c if c.is_ascii() => data.push(c as u8),
            _ => return None,
        }
    }
    if data.len() % 4 == 0 {
        for _ in 0..2 {
            if data.last() == Some(&b'=') {
                data.pop();
            }
        }
    }
    if data.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let (mut buffer, mut bits) = (0u32, 0);
    for c in data {
        let v = ALPHABET.iter().position(|&a| a == c)? as u32;
        buffer = buffer << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

/// `decodeURIComponent`.
fn uri_component_decode(s: &str) -> Result<String> {
    let malformed = || DecodeError::new("URI malformed");
    let b = s.as_bytes();
    let hex_byte = |i: usize| -> Option<u8> {
        if i + 2 < b.len() && b[i] == b'%' {
            let hi = (b[i + 1] as char).to_digit(16)?;
            let lo = (b[i + 2] as char).to_digit(16)?;
            Some((hi << 4 | lo) as u8)
        } else {
            None
        }
    };
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut copied = 0;
    while i < b.len() {
        if b[i] != b'%' {
            i += 1;
            continue;
        }
        out.push_str(&s[copied..i]);
        let first = hex_byte(i).ok_or_else(malformed)?;
        i += 3;
        if first < 0x80 {
            // `decodeURIComponent` decodes reserved characters too.
            out.push(first as char);
            copied = i;
            continue;
        }
        let count = match first {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => return Err(malformed()),
        };
        let mut bytes = vec![first];
        for _ in 1..count {
            let byte = hex_byte(i)
                .filter(|b| b & 0xC0 == 0x80)
                .ok_or_else(malformed)?;
            bytes.push(byte);
            i += 3;
        }
        out.push_str(std::str::from_utf8(&bytes).map_err(|_| malformed())?);
        copied = i;
    }
    out.push_str(&s[copied..]);
    Ok(out)
}
