//! `ZeroOptions::default()` waits JS's 5 minutes (`hiddenTabDisconnectDelay`)
//! after `set_visible(false)` before it disconnects. The conformance vectors
//! set the delay themselves, so they don't check the default.
//!
//! `Zero::pending_settled`, which an app awaits before hiding the client:
//! it ends at the first of the server confirming the pending mutations, the
//! server being unable to, and the timeout.

use std::rc::Rc;
use std::sync::Arc;
use zero_client::client::{ConnectionState, Deferred, Mutator, Transaction, Zero, ZeroOptions};
use zero_client::json::{self, Object};
use zero_client::schema::Schema;
use zero_client::testing::{ManualScheduler, MockSocket, MockSocketFactory, RecordingLogSink};

#[test]
fn a_hidden_client_disconnects_after_five_minutes_by_default() {
    let schema = Schema::from_json(
        &json::parse(
            r#"{"tables": {"issue": {"name": "issue", "primaryKey": ["id"],
                "columns": {"id": {"type": "string"}}}}}"#,
        )
        .unwrap(),
    )
    .unwrap();
    let scheduler = ManualScheduler::new(1_700_000_000_000.0);
    let factory = MockSocketFactory::new();
    let options = ZeroOptions {
        cache_url: Some("http://zero.test".into()),
        user_id: Some("u1".into()),
        log_sink: Some(Rc::new(RecordingLogSink::default())),
        ..ZeroOptions::default()
    };
    let zero: Zero = Zero::new(
        Arc::new(schema),
        (),
        vec![],
        options,
        scheduler.clone(),
        Some(factory.clone()),
    )
    .unwrap();
    let settle = || {
        zero.dispatch(|| {});
        scheduler.run_due_timers();
    };
    settle();
    let socket = factory.sockets().last().cloned().expect("a socket");
    socket.receive_text(r#"["connected",{"wsid":"w1"}]"#);
    settle();
    assert_eq!(zero.connection().state(), ConnectionState::Connected);

    zero.set_visible(false);
    settle();
    // Answer pings, so the connection stays up while the app is hidden.
    let mut waited = 0.0;
    while waited < 299_000.0 {
        scheduler.advance(1_000.0);
        waited += 1_000.0;
        socket.receive_text(r#"["pong",{}]"#);
        settle();
    }
    assert_eq!(zero.connection().state(), ConnectionState::Connected);

    scheduler.advance(1_000.0);
    settle();
    assert!(
        matches!(
            zero.connection().state(),
            ConnectionState::Disconnected { .. }
        ),
        "{:?}",
        zero.connection().state()
    );
}

/// A connected-or-not client with one mutator, on a manual clock.
struct Client {
    zero: Zero,
    scheduler: Rc<ManualScheduler>,
    factory: Rc<MockSocketFactory>,
}

impl Client {
    fn new() -> Self {
        let schema = Schema::from_json(
            &json::parse(
                r#"{"tables": {"issue": {"name": "issue", "primaryKey": ["id"],
                    "columns": {"id": {"type": "string"}}}}}"#,
            )
            .unwrap(),
        )
        .unwrap();
        let create = Mutator::new("issue.create", |tx: &Transaction, args, _: &()| {
            let mut row = Object::new();
            row.insert("id", args.unwrap().get("id").unwrap().clone());
            tx.table("issue").insert(&row)?;
            Ok(None)
        });
        let scheduler = ManualScheduler::new(1_700_000_000_000.0);
        let factory = MockSocketFactory::new();
        let options = ZeroOptions {
            cache_url: Some("http://zero.test".into()),
            user_id: Some("u1".into()),
            log_sink: Some(Rc::new(RecordingLogSink::default())),
            ..ZeroOptions::default()
        };
        let zero = Zero::new(
            Arc::new(schema),
            (),
            vec![create],
            options,
            scheduler.clone(),
            Some(factory.clone()),
        )
        .unwrap();
        let c = Client {
            zero,
            scheduler,
            factory,
        };
        c.settle();
        c
    }

    fn settle(&self) {
        self.zero.dispatch(|| {});
        self.scheduler.run_due_timers();
    }

    fn connect(&self) -> Rc<MockSocket> {
        let socket = self.factory.sockets().last().cloned().expect("a socket");
        socket.receive_text(r#"["connected",{"wsid":"w1"}]"#);
        self.settle();
        assert_eq!(self.zero.connection().state(), ConnectionState::Connected);
        socket
    }

    fn create(&self, id: &str) {
        self.zero.mutate(
            "issue.create",
            Some(json::parse(&format!(r#"{{"id":"{id}"}}"#)).unwrap()),
        );
        self.settle();
    }

    /// Moves the clock forward by `ms`, and runs what that queued.
    fn advance(&self, ms: f64) {
        self.scheduler.advance(ms);
        self.settle();
    }
}

/// What `pending_settled` resolved with, once it has.
fn outcome(settled: &Deferred<bool>) -> Option<bool> {
    settled
        .result()
        .map(|r| r.expect("pending_settled never fails"))
}

/// With nothing pending, `pending_settled` resolves at once, and leaves no
/// timer behind.
#[test]
fn pending_settled_resolves_at_once_with_nothing_pending() {
    let c = Client::new();
    c.connect();
    assert_eq!(outcome(&c.zero.pending_settled(None)), Some(true));
    let timers = c.scheduler.pending_timers();
    assert_eq!(outcome(&c.zero.pending_settled(Some(2_000.0))), Some(true));
    assert_eq!(c.scheduler.pending_timers(), timers);
}

/// A pending mutation is still unsettled partway into the timeout, and the
/// poke that confirms it settles the wait, with none pending.
#[test]
fn pending_settled_resolves_once_the_server_confirms() {
    let c = Client::new();
    let socket = c.connect();
    c.create("i1");
    // Waits less than the ping interval: an unanswered ping would
    // disconnect, which settles it too.
    let settled = c.zero.pending_settled(Some(2_000.0));
    c.advance(1_000.0);
    assert_eq!(outcome(&settled), None);

    let cid = c.zero.client_id();
    socket.receive_text(r#"["pokeStart",{"pokeID":"01","baseCookie":null}]"#);
    socket.receive_text(&format!(
        r#"["pokePart",{{"pokeID":"01","lastMutationIDChanges":{{"{cid}":1}}}}]"#
    ));
    socket.receive_text(r#"["pokeEnd",{"pokeID":"01","cookie":"01"}]"#);
    c.settle();
    assert_eq!(outcome(&settled), Some(true));
}

/// The server doesn't confirm the mutation in time: the wait ends at the
/// timeout, on the client's scheduler, with the mutation still pending.
#[test]
fn pending_settled_gives_up_at_the_timeout() {
    let c = Client::new();
    c.connect();
    c.create("i1");
    let settled = c.zero.pending_settled(Some(2_000.0));
    c.advance(1_999.0);
    assert_eq!(outcome(&settled), None);
    c.advance(1.0);
    assert_eq!(outcome(&settled), Some(false));
    assert_eq!(c.zero.connection().state(), ConnectionState::Connected);
}

/// Not connected, the server can't confirm a pending mutation: the wait
/// ends at once.
#[test]
fn pending_settled_resolves_at_once_when_not_connected() {
    let c = Client::new();
    c.create("i1");
    assert_ne!(c.zero.connection().state(), ConnectionState::Connected);
    assert_eq!(outcome(&c.zero.pending_settled(Some(2_000.0))), Some(false));
}

/// Closing the client ends the wait, with the mutation still pending.
#[test]
fn pending_settled_resolves_on_close() {
    let c = Client::new();
    c.connect();
    c.create("i1");
    let settled = c.zero.pending_settled(None);
    c.advance(1_000.0);
    assert_eq!(outcome(&settled), None);
    c.zero.close();
    assert_eq!(outcome(&settled), Some(false));
}
