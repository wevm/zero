//! Messages from the client to zero-cache (`upstreamSchema` in
//! `packages/zero-protocol/src/up.ts`).

use super::*;

#[derive(Clone, Debug, PartialEq)]
pub enum Upstream {
    InitConnection(InitConnectionBody),
    Ping,
    DeleteClients(DeleteClientsBody),
    ChangeDesiredQueries(ChangeDesiredQueriesBody),
    Pull(PullRequestBody),
    UpdateAuth(UpdateAuthBody),
    Push(PushBody),
    /// Sent before the client closes the socket. The body is any array.
    CloseConnection(Vec<Value>),
    Inspect(InspectUpBody),
    /// Tells zero-cache the client has seen the mutation results up to the
    /// given ID, so it can delete them.
    AckMutationResponses(MutationId),
    /// A message whose string tag this build doesn't know, kept as sent.
    /// `upstreamSchema` is an open `taggedTupleUnion`, so a newer client's
    /// messages parse.
    Unrecognized(Vec<Value>),
}

impl Upstream {
    pub fn tag(&self) -> &str {
        match self {
            Upstream::InitConnection(_) => "initConnection",
            Upstream::Ping => "ping",
            Upstream::DeleteClients(_) => "deleteClients",
            Upstream::ChangeDesiredQueries(_) => "changeDesiredQueries",
            Upstream::Pull(_) => "pull",
            Upstream::UpdateAuth(_) => "updateAuth",
            Upstream::Push(_) => "push",
            Upstream::CloseConnection(_) => "closeConnection",
            Upstream::Inspect(_) => "inspect",
            Upstream::AckMutationResponses(_) => "ackMutationResponses",
            Upstream::Unrecognized(a) => a.first().and_then(Value::as_str).unwrap_or(""),
        }
    }

    /// Parses message text, as zero-cache receives it.
    pub fn from_text(text: &str) -> Result<Self> {
        Self::from_json(&parse_message(text)?)
    }

    /// The message text to send.
    pub fn to_text(&self) -> String {
        json::stringify(&self.to_json())
    }
}

impl JsonCodable for Upstream {
    fn from_json(v: &Value) -> Result<Self> {
        let Value::Array(a) = v else {
            return Err(DecodeError::new("Expected array"));
        };
        let Some(Value::String(tag)) = a.first() else {
            return Err(DecodeError::new("Expected string").at("0"));
        };
        // A known tag picks its tuple, which must have exactly two elements.
        fn body<'a, T>(
            v: &'a Value,
            tag: &str,
            decode: impl FnOnce(&'a Value) -> Result<T>,
        ) -> Result<T> {
            decode(body_of(v, tag)?).map_err(|e| e.at("1"))
        }
        Ok(match tag.as_str() {
            "initConnection" => {
                Upstream::InitConnection(body(v, tag, InitConnectionBody::from_json)?)
            }
            "ping" => {
                body(v, tag, decode::object)?;
                Upstream::Ping
            }
            "deleteClients" => Upstream::DeleteClients(body(v, tag, DeleteClientsBody::from_json)?),
            "changeDesiredQueries" => {
                Upstream::ChangeDesiredQueries(body(v, tag, ChangeDesiredQueriesBody::from_json)?)
            }
            "pull" => Upstream::Pull(body(v, tag, PullRequestBody::from_json)?),
            "updateAuth" => Upstream::UpdateAuth(body(v, tag, UpdateAuthBody::from_json)?),
            "push" => Upstream::Push(body(v, tag, PushBody::from_json)?),
            "closeConnection" => {
                Upstream::CloseConnection(body(v, tag, |b| Ok(decode::array_value(b)?.clone()))?)
            }
            "inspect" => Upstream::Inspect(body(v, tag, InspectUpBody::from_json)?),
            "ackMutationResponses" => {
                Upstream::AckMutationResponses(body(v, tag, MutationId::from_json)?)
            }
            _ => Upstream::Unrecognized(a.clone()),
        })
    }

    fn to_json(&self) -> Value {
        let body = match self {
            Upstream::InitConnection(b) => b.to_json(),
            Upstream::Ping => Value::Object(Object::new()),
            Upstream::DeleteClients(b) => b.to_json(),
            Upstream::ChangeDesiredQueries(b) => b.to_json(),
            Upstream::Pull(b) => b.to_json(),
            Upstream::UpdateAuth(b) => b.to_json(),
            Upstream::Push(b) => b.to_json(),
            Upstream::CloseConnection(b) => Value::Array(b.clone()),
            Upstream::Inspect(b) => b.to_json(),
            Upstream::AckMutationResponses(id) => id.to_json(),
            Upstream::Unrecognized(a) => return Value::Array(a.clone()),
        };
        message(self.tag(), body)
    }
}
