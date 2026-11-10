//! Messages from zero-cache to the client (`downstreamSchema` in
//! `packages/zero-protocol/src/down.ts`).

use super::*;

/// A message from zero-cache. The union is closed: the client treats a
/// message with a tag it doesn't know as invalid and disconnects.
#[derive(Clone, Debug, PartialEq)]
pub enum Downstream {
    Connected(ConnectedBody),
    Error(ErrorBody),
    Pong,
    PokeStart(PokeStartBody),
    PokePart(PokePartBody),
    PokeEnd(PokeEndBody),
    Pull(PullResponseBody),
    DeleteClients(DeleteClientsBody),
    PushResponse(PushResponseBody),
    Inspect(InspectDownBody),
    /// Custom queries the API server failed to transform.
    TransformError(Vec<ErroredQuery>),
}

impl Downstream {
    pub fn tag(&self) -> &'static str {
        match self {
            Downstream::Connected(_) => "connected",
            Downstream::Error(_) => "error",
            Downstream::Pong => "pong",
            Downstream::PokeStart(_) => "pokeStart",
            Downstream::PokePart(_) => "pokePart",
            Downstream::PokeEnd(_) => "pokeEnd",
            Downstream::Pull(_) => "pull",
            Downstream::DeleteClients(_) => "deleteClients",
            Downstream::PushResponse(_) => "pushResponse",
            Downstream::Inspect(_) => "inspect",
            Downstream::TransformError(_) => "transformError",
        }
    }

    /// Parses message text, as the client receives it.
    pub fn from_text(text: &str) -> Result<Self> {
        Self::from_json(&parse_message(text)?)
    }

    /// The message text to send.
    pub fn to_text(&self) -> String {
        json::stringify(&self.to_json())
    }
}

impl JsonCodable for Downstream {
    fn from_json(v: &Value) -> Result<Self> {
        let Value::Array(a) = v else {
            return Err(DecodeError::new("Expected array"));
        };
        let [Value::String(tag), body] = a.as_slice() else {
            return Err(DecodeError::new("Expected [tag, body] tuple"));
        };
        let at = |e: DecodeError| e.at("1");
        Ok(match tag.as_str() {
            "connected" => Downstream::Connected(ConnectedBody::from_json(body).map_err(at)?),
            "error" => Downstream::Error(ErrorBody::from_json(body).map_err(at)?),
            "pong" => {
                decode::object(body).map_err(at)?;
                Downstream::Pong
            }
            "pokeStart" => Downstream::PokeStart(PokeStartBody::from_json(body).map_err(at)?),
            "pokePart" => Downstream::PokePart(PokePartBody::from_json(body).map_err(at)?),
            "pokeEnd" => Downstream::PokeEnd(PokeEndBody::from_json(body).map_err(at)?),
            "pull" => Downstream::Pull(PullResponseBody::from_json(body).map_err(at)?),
            "deleteClients" => {
                Downstream::DeleteClients(DeleteClientsBody::from_json(body).map_err(at)?)
            }
            "pushResponse" => {
                Downstream::PushResponse(PushResponseBody::from_json(body).map_err(at)?)
            }
            "inspect" => Downstream::Inspect(InspectDownBody::from_json(body).map_err(at)?),
            "transformError" => Downstream::TransformError(
                decode::array(body, ErroredQuery::from_json).map_err(at)?,
            ),
            _ => {
                return Err(DecodeError::new(format!("Unknown message type \"{tag}\"")).at("0"));
            }
        })
    }

    fn to_json(&self) -> Value {
        let body = match self {
            Downstream::Connected(b) => b.to_json(),
            Downstream::Error(b) => b.to_json(),
            Downstream::Pong => Value::Object(Object::new()),
            Downstream::PokeStart(b) => b.to_json(),
            Downstream::PokePart(b) => b.to_json(),
            Downstream::PokeEnd(b) => b.to_json(),
            Downstream::Pull(b) => b.to_json(),
            Downstream::DeleteClients(b) => b.to_json(),
            Downstream::PushResponse(b) => b.to_json(),
            Downstream::Inspect(b) => b.to_json(),
            Downstream::TransformError(b) => array_json(b),
        };
        message(self.tag(), body)
    }
}
