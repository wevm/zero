//! Tracks the mutations waiting for the server's verdict and settles their
//! `server` results (`packages/zero-client/src/client/mutation-tracker.ts`).

use super::replica_store::{KvDiff, KvDiffOp};
use super::{ApplicationError, ClientErrorKind, ConnectionError, Deferred, Error};
use crate::decode::JsonCodable;
use crate::json::{Value, number};
use crate::protocol::{
    ErrorBody, ErrorKind, ErrorOrigin, ErrorReason, FailureCause, MutationError, MutationId,
    MutationResult, PushError, PushResponseBody,
};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

pub(crate) type ServerResult = Deferred<Option<Value>>;

struct Entry {
    mutation_id: Option<f64>,
    server: ServerResult,
}

#[derive(Default)]
struct State {
    /// By ephemeral ID.
    outstanding: HashMap<u64, Entry>,
    /// The ephemeral IDs in insertion order, which is mutation ID order.
    /// Settled IDs stay until the head passes them.
    order: Vec<u64>,
    ephemeral_ids_by_mutation_id: BTreeMap<u64, u64>,
    next_ephemeral_id: u64,
    largest_outstanding_mutation_id: f64,
    current_mutation_id: f64,
}

pub(crate) struct MutationTracker {
    client_id: String,
    state: RefCell<State>,
    ack_mutations: Box<dyn Fn(MutationId)>,
    on_fatal_error: Box<dyn Fn(ConnectionError)>,
    on_all_mutations_applied: Box<dyn Fn()>,
}

fn mid_key(mid: f64) -> u64 {
    mid as u64
}

impl MutationTracker {
    pub(crate) fn new(
        client_id: String,
        ack_mutations: Box<dyn Fn(MutationId)>,
        on_fatal_error: Box<dyn Fn(ConnectionError)>,
        on_all_mutations_applied: Box<dyn Fn()>,
    ) -> Self {
        MutationTracker {
            client_id,
            state: RefCell::default(),
            ack_mutations,
            on_fatal_error,
            on_all_mutations_applied,
        }
    }

    pub(crate) fn size(&self) -> usize {
        self.state.borrow().outstanding.len()
    }

    pub(crate) fn track_mutation(&self) -> (u64, ServerResult) {
        let mut s = self.state.borrow_mut();
        s.next_ephemeral_id += 1;
        let id = s.next_ephemeral_id;
        let server = ServerResult::new();
        if s.outstanding.is_empty() {
            s.order.clear();
        }
        s.outstanding.insert(
            id,
            Entry {
                mutation_id: None,
                server: server.clone(),
            },
        );
        s.order.push(id);
        (id, server)
    }

    pub(crate) fn mutation_id_assigned(&self, ephemeral_id: u64, mutation_id: f64) {
        let mut s = self.state.borrow_mut();
        let Some(entry) = s.outstanding.get_mut(&ephemeral_id) else {
            return;
        };
        entry.mutation_id = Some(mutation_id);
        s.ephemeral_ids_by_mutation_id
            .insert(mid_key(mutation_id), ephemeral_id);
        s.largest_outstanding_mutation_id = s.largest_outstanding_mutation_id.max(mutation_id);
    }

    /// Rejects a mutation the client could not run. It was not stored.
    pub(crate) fn reject_mutation(&self, ephemeral_id: u64, error: &Error) {
        let app = ApplicationError::wrapping(error);
        self.settle(ephemeral_id, Err(Error::Application(app)));
    }

    /// Rejects every outstanding mutation, when the client is disconnected,
    /// in error, or closed.
    pub(crate) fn reject_all_outstanding_mutations(&self, error: &ConnectionError) {
        let ids: Vec<u64> = {
            let s = self.state.borrow();
            if s.outstanding.is_empty() {
                return;
            }
            s.order.clone()
        };
        for id in ids {
            self.settle(id, Err(Error::Connection(error.clone())));
        }
        {
            let mut s = self.state.borrow_mut();
            s.order.clear();
            s.largest_outstanding_mutation_id = s.current_mutation_id;
        }
        (self.on_all_mutations_applied)();
    }

    /// The watch of `m/<clientID>/`: mutation results zero-cache poked down.
    pub(crate) fn process_mutation_responses(&self, diff: &KvDiff) -> Result<(), ConnectionError> {
        let prefix = format!("{}{}/", crate::keys::MUTATIONS_PREFIX, self.client_id);
        let mut largest = 0.0f64;
        for op in diff {
            let suffix = &op.key()[prefix.len().min(op.key().len())..];
            let mutation_id = number::parse(suffix);
            if suffix.is_empty() || mutation_id.is_nan() {
                return Err(ConnectionError::client(
                    ClientErrorKind::Internal,
                    format!(
                        "MutationTracker received a diff with an invalid mutation ID: {}",
                        op.key()
                    ),
                ));
            }
            largest = largest.max(mutation_id);
            match op {
                KvDiffOp::Add { new, .. } => {
                    let result = MutationResult::from_json(new).map_err(|e| {
                        ConnectionError::client(ClientErrorKind::Internal, e.to_string())
                    })?;
                    match result {
                        MutationResult::Ok { data } => {
                            self.process_mutation_ok(&self.client_id, mutation_id, data)?
                        }
                        MutationResult::Error(e) => {
                            self.process_mutation_error(&self.client_id, mutation_id, &e)?
                        }
                    }
                }
                KvDiffOp::Del { .. } => {}
                KvDiffOp::Change { .. } => {
                    return Err(ConnectionError::client(
                        ClientErrorKind::Internal,
                        "MutationTracker does not expect change operations",
                    ));
                }
            }
        }
        if largest > 0.0 {
            (self.ack_mutations)(MutationId {
                id: largest,
                client_id: self.client_id.clone(),
            });
        }
        Ok(())
    }

    pub(crate) fn process_push_response(
        &self,
        response: &PushResponseBody,
    ) -> Result<(), ConnectionError> {
        match response {
            PushResponseBody::Error(e) => (self.on_fatal_error)(fatal_error(e)),
            PushResponseBody::Ok { mutations } => {
                for m in mutations {
                    match &m.result {
                        MutationResult::Ok { data } => {
                            self.process_mutation_ok(&m.id.client_id, m.id.id, data.clone())?
                        }
                        MutationResult::Error(e) => {
                            self.process_mutation_error(&m.id.client_id, m.id.id, e)?
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Resolves the mutations the server has applied (`lmidAdvanced`).
    pub(crate) fn on_connected(&self, last_mutation_id: f64) {
        let _ = self.lmid_advanced(last_mutation_id);
    }

    pub(crate) fn lmid_advanced(&self, last_mutation_id: f64) -> Result<(), String> {
        let settled: Vec<u64> = {
            let mut s = self.state.borrow_mut();
            if last_mutation_id < s.current_mutation_id {
                return Err("lmid must be greater than or equal to current lmid".into());
            }
            if last_mutation_id == s.current_mutation_id {
                return Ok(());
            }
            s.current_mutation_id = last_mutation_id;
            let mut settled = Vec::new();
            let mut head = 0;
            while head < s.order.len() {
                let id = s.order[head];
                if let Some(entry) = s.outstanding.get(&id) {
                    match entry.mutation_id {
                        Some(mid) if mid <= last_mutation_id => settled.push(id),
                        _ => break,
                    }
                }
                head += 1;
            }
            s.order.drain(..head);
            settled
        };
        for id in settled {
            self.settle(id, Ok(None));
        }
        let all = last_mutation_id >= self.state.borrow().largest_outstanding_mutation_id;
        if all {
            (self.on_all_mutations_applied)();
        }
        Ok(())
    }

    fn process_mutation_error(
        &self,
        client_id: &str,
        mid: f64,
        error: &MutationError,
    ) -> Result<(), ConnectionError> {
        if client_id != self.client_id {
            return Err(ConnectionError::client(
                ClientErrorKind::Internal,
                "received mutation for the wrong client",
            ));
        }
        let kind = error.error.as_str().unwrap_or("");
        if kind == "alreadyProcessed" {
            return Ok(());
        }
        let failure = if kind == "app" {
            Error::Application(ApplicationError::with_details(
                error
                    .message
                    .as_ref()
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown application error: app"),
                // JS keeps details only when truthy.
                error.details.clone().filter(is_truthy),
            ))
        } else {
            let message = if kind == "oooMutation" {
                "Server reported an out-of-order mutation".to_owned()
            } else {
                format!(
                    "Unknown fallback error with mutation ID {}: {kind}",
                    number::format(mid)
                )
            };
            Error::Connection(invalid_push(message))
        };
        if let Some(ephemeral_id) = self.ephemeral_id(mid) {
            self.settle(ephemeral_id, Err(failure));
        }
        if kind == "oooMutation" {
            (self.on_fatal_error)(invalid_push(
                "Server reported an out-of-order mutation".to_owned(),
            ));
        }
        Ok(())
    }

    fn process_mutation_ok(
        &self,
        client_id: &str,
        mid: f64,
        data: Option<Value>,
    ) -> Result<(), ConnectionError> {
        if client_id != self.client_id {
            return Err(ConnectionError::client(
                ClientErrorKind::Internal,
                "received mutation for the wrong client",
            ));
        }
        if let Some(ephemeral_id) = self.ephemeral_id(mid) {
            self.settle(ephemeral_id, Ok(data));
        }
        Ok(())
    }

    fn ephemeral_id(&self, mid: f64) -> Option<u64> {
        let s = self.state.borrow();
        let id = *s.ephemeral_ids_by_mutation_id.get(&mid_key(mid))?;
        s.outstanding.contains_key(&id).then_some(id)
    }

    fn settle(&self, ephemeral_id: u64, result: Result<Option<Value>, Error>) {
        let entry = {
            let mut s = self.state.borrow_mut();
            let Some(entry) = s.outstanding.remove(&ephemeral_id) else {
                return;
            };
            if let Some(mid) = entry.mutation_id {
                s.ephemeral_ids_by_mutation_id.remove(&mid_key(mid));
            }
            entry
        };
        entry.server.settle(result);
    }
}

fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null | Value::Bool(false) => false,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => *n != 0.0 && !n.is_nan(),
        _ => true,
    }
}

fn invalid_push(message: String) -> ConnectionError {
    ConnectionError::Server(Box::new(ErrorBody::Basic {
        kind: ErrorKind::InvalidPush,
        message,
        origin: Some(ErrorOrigin::Server),
    }))
}

fn fatal_error(e: &PushError) -> ConnectionError {
    let internal = FailureCause::ZeroCache {
        reason: ErrorReason::Internal,
    };
    let (message, cause) = match e {
        PushError::UnsupportedPushVersion { .. } => {
            ("Unsupported push version".to_owned(), internal)
        }
        PushError::UnsupportedSchemaVersion { .. } => {
            ("Unsupported schema version".to_owned(), internal)
        }
        PushError::Http {
            status, details, ..
        } => (
            format!(
                "Fetch from API server returned non-OK status {}: {details}",
                number::format(*status)
            ),
            FailureCause::Http {
                status: *status,
                body_preview: None,
            },
        ),
        PushError::ZeroPusher { details, .. } => (format!("ZeroPusher error: {details}"), internal),
    };
    ConnectionError::Server(Box::new(ErrorBody::PushFailed {
        message,
        mutation_ids: Vec::new(),
        details: None,
        cause,
    }))
}
