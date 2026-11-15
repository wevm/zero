//! `Zero::add_mutation_outcome_listener`, which zero-swift has and JS
//! doesn't: each mutation's server outcome is heard once, including that of
//! a mutation replayed after its `server` result gave up on it.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use zero_client::client::{
    MutationOutcome, MutationServerOutcome, Mutator, MutatorResult, Transaction, Zero, ZeroOptions,
};
use zero_client::json::{self, Object, Value};
use zero_client::schema::Schema;
use zero_client::testing::{ManualScheduler, MockSocket, MockSocketFactory, RecordingLogSink};

struct Harness {
    zero: Zero,
    scheduler: Rc<ManualScheduler>,
    factory: Rc<MockSocketFactory>,
    heard: Rc<RefCell<Vec<MutationServerOutcome>>>,
}

impl Harness {
    fn new(disconnect_timeout_ms: f64) -> Self {
        let schema = Schema::from_json(
            &json::parse(
                r#"{"tables": {"issue": {"name": "issue", "primaryKey": ["id"],
                    "columns": {"id": {"type": "string"}, "title": {"type": "string"}}}}}"#,
            )
            .unwrap(),
        )
        .unwrap();
        let create = Mutator::new("issue.create", |tx: &Transaction, args, _: &()| {
            let args = args.unwrap();
            let mut row = Object::new();
            row.insert("id", args.get("id").unwrap().clone());
            row.insert("title", args.get("title").unwrap().clone());
            tx.table("issue").insert(&row)?;
            Ok(None)
        });
        let scheduler = ManualScheduler::new(1_700_000_000_000.0);
        let factory = MockSocketFactory::new();
        let options = ZeroOptions {
            cache_url: Some("http://zero.test".into()),
            user_id: Some("u1".into()),
            disconnect_timeout_ms,
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
        let heard = Rc::new(RefCell::new(Vec::new()));
        let h = Harness {
            zero,
            scheduler,
            factory,
            heard,
        };
        h.settle();
        h
    }

    /// Adds a listener that records what it hears in `heard`.
    fn listen(&self, heard: &Rc<RefCell<Vec<MutationServerOutcome>>>) -> Box<dyn Fn()> {
        let heard = heard.clone();
        self.zero
            .add_mutation_outcome_listener(move |o| heard.borrow_mut().push(o.clone()))
    }

    fn settle(&self) {
        self.zero.dispatch(|| {});
        self.scheduler.run_due_timers();
    }

    fn socket(&self) -> Rc<MockSocket> {
        self.factory.sockets().last().cloned().expect("a socket")
    }

    fn connect(&self) -> Rc<MockSocket> {
        let socket = self.socket();
        socket.receive_text(r#"["connected",{"wsid":"w1"}]"#);
        self.settle();
        socket
    }

    fn create(&self, id: &str, title: &str) -> MutatorResult {
        let r = self.zero.mutate("issue.create", Some(issue(id, title)));
        self.settle();
        r
    }

    /// A poke from `base` to `cookie` with `part`'s fields.
    fn poke(&self, socket: &MockSocket, base: &str, cookie: &str, part: &str) {
        socket.receive_text(&format!(
            r#"["pokeStart",{{"pokeID":"{cookie}","baseCookie":{base}}}]"#
        ));
        socket.receive_text(&format!(r#"["pokePart",{{"pokeID":"{cookie}",{part}}}]"#));
        socket.receive_text(&format!(
            r#"["pokeEnd",{{"pokeID":"{cookie}","cookie":"{cookie}"}}]"#
        ));
        self.settle();
    }

    /// The IDs of the mutations `socket` pushed, in order.
    fn pushed(&self, socket: &MockSocket) -> Vec<f64> {
        socket
            .messages()
            .iter()
            .filter(|m| m.starts_with(r#"["push""#))
            .flat_map(|m| {
                let v = json::parse(m).unwrap();
                let ms = v.at(1).unwrap().get("mutations").unwrap().clone();
                let Value::Array(ms) = ms else { panic!() };
                ms.iter()
                    .map(|m| m.get("id").unwrap().as_f64().unwrap())
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

fn issue(id: &str, title: &str) -> Value {
    json::parse(&format!(r#"{{"id":"{id}","title":"{title}"}}"#)).unwrap()
}

fn heard(
    mutation_id: u64,
    id: &str,
    title: &str,
    outcome: MutationOutcome,
) -> MutationServerOutcome {
    MutationServerOutcome {
        mutation_id,
        name: "issue.create".into(),
        args: Some(issue(id, title)),
        outcome,
    }
}

fn server(r: &MutatorResult) -> MutationOutcome {
    r.server.result().expect("settled").unwrap()
}

fn app_error(message: &str, details: Option<&str>) -> MutationOutcome {
    MutationOutcome::AppError {
        message: message.into(),
        details: details.map(|d| json::parse(d).unwrap()),
    }
}

fn success(data: Option<&str>) -> MutationOutcome {
    MutationOutcome::Success {
        data: data.map(|d| json::parse(d).unwrap()),
    }
}

/// A push response reports one mutation's data and another's app error,
/// and the poke that confirms all three reports the third's success. The
/// first two aren't heard again when the poke confirms them too.
#[test]
fn hears_each_server_outcome_once() {
    let h = Harness::new(60_000.0);
    let _remove = h.listen(&h.heard);
    let socket = h.connect();
    let cid = h.zero.client_id();
    let one = h.create("i1", "one");
    let two = h.create("i2", "two");
    let three = h.create("i3", "three");
    assert_eq!(h.pushed(&socket), [1.0, 2.0, 3.0]);
    assert!(h.heard.borrow().is_empty());

    socket.receive_text(&format!(
        r#"["pushResponse",{{"mutations":[
            {{"id":{{"clientID":"{cid}","id":1}},"result":{{"data":{{"n":1}}}}}},
            {{"id":{{"clientID":"{cid}","id":2}},"result":{{"error":"app","message":"taken","details":{{"code":7}}}}}}
        ]}}]"#
    ));
    h.settle();
    h.poke(
        &socket,
        "null",
        "01",
        &format!(r#""lastMutationIDChanges":{{"{cid}":3}}"#),
    );

    assert_eq!(server(&one), success(Some(r#"{"n":1}"#)));
    assert_eq!(server(&two), app_error("taken", Some(r#"{"code":7}"#)));
    assert_eq!(server(&three), success(None));
    assert_eq!(
        *h.heard.borrow(),
        [
            heard(1, "i1", "one", success(Some(r#"{"n":1}"#))),
            heard(2, "i2", "two", app_error("taken", Some(r#"{"code":7}"#))),
            heard(3, "i3", "three", success(None)),
        ]
    );
}

/// The client gives up on the server, so `server` settles with a
/// `ZeroError`; it reconnects, sends the two mutations again, and the
/// server rejects one and applies the other. The listener hears those, not
/// the `ZeroError`s.
#[test]
fn hears_the_outcome_of_a_mutation_replayed_after_the_disconnect_timeout() {
    let h = Harness::new(5_000.0);
    let _remove = h.listen(&h.heard);
    let first = h.connect();
    let cid = h.zero.client_id();
    let one = h.create("i1", "one");
    let two = h.create("i2", "two");
    assert_eq!(h.pushed(&first), [1.0, 2.0]);

    first.server_close(1006, "", false);
    h.settle();
    for _ in 0..10 {
        h.scheduler.advance(1_000.0);
        h.settle();
    }
    assert!(matches!(server(&one), MutationOutcome::ZeroError { .. }));
    assert!(matches!(server(&two), MutationOutcome::ZeroError { .. }));
    assert!(h.heard.borrow().is_empty());

    let sockets = h.factory.sockets().len();
    for _ in 0..600 {
        if h.factory.sockets().len() > sockets {
            break;
        }
        h.scheduler.advance(100.0);
        h.settle();
    }
    let second = h.connect();
    assert!(!Rc::ptr_eq(&first, &second), "the client reconnected");
    assert_eq!(h.pushed(&second), [1.0, 2.0], "the mutations are replayed");
    assert!(h.heard.borrow().is_empty());

    h.poke(
        &second,
        "null",
        "01",
        &format!(
            r#""lastMutationIDChanges":{{"{cid}":2}},"mutationsPatch":[{{"op":"put","mutation":{{"id":{{"clientID":"{cid}","id":1}},"result":{{"error":"app","message":"taken"}}}}}}]"#
        ),
    );
    assert_eq!(
        *h.heard.borrow(),
        [
            heard(1, "i1", "one", app_error("taken", None)),
            heard(2, "i2", "two", success(None)),
        ]
    );
}

/// A removed listener hears nothing more; the others still do.
#[test]
fn a_removed_listener_hears_nothing() {
    let h = Harness::new(60_000.0);
    let removed = Rc::new(RefCell::new(Vec::new()));
    let remove = h.listen(&removed);
    let _keep = h.listen(&h.heard);
    let socket = h.connect();
    let cid = h.zero.client_id();
    h.create("i1", "one");
    h.poke(
        &socket,
        "null",
        "01",
        &format!(r#""lastMutationIDChanges":{{"{cid}":1}}"#),
    );
    assert_eq!(*removed.borrow(), [heard(1, "i1", "one", success(None))]);

    remove();
    remove();
    h.create("i2", "two");
    h.poke(
        &socket,
        r#""01""#,
        "02",
        &format!(r#""lastMutationIDChanges":{{"{cid}":2}}"#),
    );
    assert_eq!(*removed.borrow(), [heard(1, "i1", "one", success(None))]);
    assert_eq!(
        *h.heard.borrow(),
        [
            heard(1, "i1", "one", success(None)),
            heard(2, "i2", "two", success(None)),
        ]
    );
}

/// The listeners stay with the client when it replaces itself, after the
/// server lost its state, and hear the new client's mutations.
#[test]
fn listeners_outlive_a_replaced_client() {
    let h = Harness::new(60_000.0);
    let _remove = h.listen(&h.heard);
    let socket = h.connect();
    let old = h.zero.client_id();
    socket.receive_text(r#"["error",{"kind":"ClientNotFound","message":"gone"}]"#);
    h.settle();
    let cid = h.zero.client_id();
    assert_ne!(cid, old, "the client replaced itself");

    let socket = h.connect();
    h.create("i1", "one");
    assert_eq!(h.pushed(&socket), [1.0]);
    h.poke(
        &socket,
        "null",
        "01",
        &format!(r#""lastMutationIDChanges":{{"{cid}":1}}"#),
    );
    assert_eq!(*h.heard.borrow(), [heard(1, "i1", "one", success(None))]);
}
