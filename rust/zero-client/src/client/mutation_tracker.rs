//! Tracks the mutations waiting for the server's verdict and settles their
//! `server` results (`packages/zero-client/src/client/mutation-tracker.ts`).

use super::engine::{Engine, MutationOutcome, MutationServerOutcome};
use super::replica_store::{KvDiff, KvDiffOp};
use super::{ApplicationError, ClientErrorKind, ConnectionError, Deferred, Error};
use crate::decode::JsonCodable;
use crate::json::{Value, number};
use crate::protocol::{
    ErrorBody, ErrorKind, ErrorOrigin, ErrorReason, FailureCause, MutationError, MutationId,
    MutationResult, PushError, PushResponseBody,
};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

pub(crate) type ServerResult = Deferred<Option<Value>>;

type OutcomeListener = Rc<dyn Fn(&MutationServerOutcome)>;

/// The listeners of `Zero::add_mutation_outcome_listener`. The client
/// keeps them, and gives them to each engine it makes, so they outlive a
/// replaced one.
#[derive(Default)]
pub(crate) struct OutcomeListeners {
    listeners: RefCell<Vec<(u64, OutcomeListener)>>,
    next_id: Cell<u64>,
}

impl OutcomeListeners {
    pub(crate) fn add(&self, listener: OutcomeListener) -> u64 {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        self.listeners.borrow_mut().push((id, listener));
        id
    }

    pub(crate) fn remove(&self, id: u64) {
        self.listeners.borrow_mut().retain(|(i, _)| *i != id);
    }

    fn notify(&self, outcome: &MutationServerOutcome) {
        // A listener may add or remove one.
        let listeners: Vec<_> = self
            .listeners
            .borrow()
            .iter()
            .map(|(_, l)| l.clone())
            .collect();
        for l in listeners {
            l(outcome);
        }
    }
}

/// A mutation taken off the unreported ones, to report to `listeners`
/// once its `server` result has settled.
struct Report {
    listeners: Rc<OutcomeListeners>,
    mutation_id: u64,
    name: String,
    args: Option<Value>,
}

impl Report {
    fn send(self, outcome: MutationOutcome) {
        self.listeners.notify(&MutationServerOutcome {
            mutation_id: self.mutation_id,
            name: self.name,
            args: self.args,
            outcome,
        });
    }
}

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
    /// The mutations sent to the server whose outcome hasn't been
    /// reported: their names and arguments, by mutation ID. Unlike
    /// `outstanding`, they stay when the connection gives up on them:
    /// they're sent again on reconnect, and their outcome is reported then.
    unreported: BTreeMap<u64, (String, Option<Value>)>,
}

pub(crate) struct MutationTracker {
    client_id: String,
    state: RefCell<State>,
    /// Hear each mutation's outcome from the server once.
    outcome_listeners: RefCell<Option<Rc<OutcomeListeners>>>,
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
            outcome_listeners: RefCell::new(None),
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

    /// Mutation `mutation_id` ran on the client and is stored: its outcome
    /// is reported once the server gives one. Swift records it when the ID
    /// is assigned and drops it when the mutator throws (`rejectMutation`);
    /// recording it once the mutator ran is the same, and moves `args` here
    /// instead of copying them.
    pub(crate) fn mutation_ran(&self, mutation_id: f64, name: &str, args: Option<Value>) {
        self.state
            .borrow_mut()
            .unreported
            .insert(mid_key(mutation_id), (name.to_owned(), args));
    }

    /// The listeners to report outcomes to, from now on.
    pub(crate) fn set_outcome_listeners(&self, listeners: Rc<OutcomeListeners>) {
        *self.outcome_listeners.borrow_mut() = Some(listeners);
    }

    /// Takes mutation `mid` off the unreported ones: its report, when it
    /// wasn't reported yet and someone listens.
    fn take_report(&self, mid: u64) -> Option<Report> {
        let (name, args) = self.state.borrow_mut().unreported.remove(&mid)?;
        let listeners = self.outcome_listeners.borrow().clone()?;
        if listeners.listeners.borrow().is_empty() {
            return None;
        }
        Some(Report {
            listeners,
            mutation_id: mid,
            name,
            args,
        })
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
            drop(s);
            // Applied without a result of their own: they succeeded.
            let lmid = mid_key(last_mutation_id);
            loop {
                let next = {
                    let s = self.state.borrow();
                    match s.unreported.first_key_value() {
                        Some((&mid, _)) if mid <= lmid => mid,
                        _ => break,
                    }
                };
                if let Some(report) = self.take_report(next) {
                    report.send(MutationOutcome::Success { data: None });
                }
            }
            let mut s = self.state.borrow_mut();
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
        let report = self
            .take_report(mid_key(mid))
            .map(|r| (r, Engine::outcome(&failure)));
        if let Some(ephemeral_id) = self.ephemeral_id(mid) {
            self.settle(ephemeral_id, Err(failure));
        }
        if let Some((report, outcome)) = report {
            report.send(outcome);
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
        let report = self.take_report(mid_key(mid)).map(|r| (r, data.clone()));
        if let Some(ephemeral_id) = self.ephemeral_id(mid) {
            self.settle(ephemeral_id, Ok(data));
        }
        if let Some((report, data)) = report {
            report.send(MutationOutcome::Success { data });
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
