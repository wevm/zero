//! The inspector's RPCs where JS has nothing to compare with (the
//! conformance vectors cover the rest): a request waiting on a connection
//! that drops or a client that closes fails, and a refused request without
//! an accepted password says how to authenticate.

use std::rc::Rc;
use std::sync::Arc;
use zero_client::client::{Deferred, Zero, ZeroOptions};
use zero_client::json::{self, Value};
use zero_client::schema::Schema;
use zero_client::testing::{ManualScheduler, MockSocket, MockSocketFactory, RecordingLogSink};

struct Harness {
    zero: Zero,
    scheduler: Rc<ManualScheduler>,
    factory: Rc<MockSocketFactory>,
}

impl Harness {
    fn new() -> Self {
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
        let zero = Zero::new(
            Arc::new(schema),
            (),
            vec![],
            options,
            scheduler.clone(),
            Some(factory.clone()),
        )
        .unwrap();
        let h = Harness {
            zero,
            scheduler,
            factory,
        };
        h.settle();
        h
    }

    fn settle(&self) {
        self.zero.dispatch(|| {});
        self.scheduler.run_due_timers();
    }

    fn socket(&self) -> Rc<MockSocket> {
        self.factory.sockets().last().cloned().expect("a socket")
    }

    fn connect(&self) {
        self.socket().receive_text(r#"["connected",{"wsid":"w1"}]"#);
        self.settle();
    }

    /// The last inspector request sent: its `op` and `id`.
    fn last_request(&self) -> (String, String) {
        let m = self
            .socket()
            .messages()
            .into_iter()
            .rev()
            .find(|m| m.starts_with(r#"["inspect""#))
            .expect("an inspect request");
        let v = json::parse(&m).unwrap();
        let body = v.at(1).unwrap();
        let s = |k: &str| body.get(k).and_then(Value::as_str).unwrap().to_owned();
        (s("op"), s("id"))
    }

    fn answer(&self, id: &str, op: &str, value: &str) {
        self.socket().receive_text(&format!(
            r#"["inspect",{{"op":"{op}","id":"{id}","value":{value}}}]"#
        ));
        self.settle();
    }
}

fn rejection<T: Clone + 'static>(d: &Deferred<T>) -> String {
    match d.result() {
        Some(Err(e)) => e.message(),
        Some(Ok(_)) => panic!("resolved"),
        None => panic!("pending"),
    }
}

#[test]
fn a_sent_request_fails_when_the_connection_drops() {
    let h = Harness::new();
    h.connect();
    let version = h.zero.inspector().server_version();
    h.settle();
    assert_eq!(h.last_request().0, "version");
    h.socket().server_close(1006, "", false);
    h.settle();
    let message = rejection(&version);
    assert!(
        message.contains("the connection to zero-cache closed"),
        "{message}"
    );
}

#[test]
fn a_request_waits_for_the_connection_and_fails_when_the_client_closes() {
    let h = Harness::new();
    let version = h.zero.inspector().server_version();
    h.settle();
    assert!(!version.is_settled(), "waits for the connection");
    h.zero.close();
    h.settle();
    assert_eq!(rejection(&version), "Zero instance closed by user");
    // And one made once closed fails at once.
    let late = h.zero.inspector().metrics();
    h.settle();
    assert_eq!(rejection(&late), "Zero instance closed by user");
}

#[test]
fn a_refused_request_says_to_authenticate() {
    let h = Harness::new();
    h.connect();
    let version = h.zero.inspector().server_version();
    h.settle();
    let (_, id) = h.last_request();
    h.answer(&id, "authenticated", "false");
    let message = rejection(&version);
    assert!(message.starts_with("Authentication failed"), "{message}");
    assert!(message.contains("authenticate(password)"), "{message}");

    // Once a password is accepted, a refused request presents it again.
    let inspector = h.zero.inspector();
    let ok = inspector.authenticate("pw");
    h.settle();
    let (op, id) = h.last_request();
    assert_eq!(op, "authenticate");
    h.answer(&id, "authenticated", "true");
    assert!(matches!(ok.result(), Some(Ok(true))));
    let version = inspector.server_version();
    h.settle();
    h.answer(&h.last_request().1, "authenticated", "false");
    let (op, id) = h.last_request();
    assert_eq!(op, "authenticate");
    h.answer(&id, "authenticated", "true");
    let (op, id) = h.last_request();
    assert_eq!(op, "version");
    h.answer(&id, "version", r#""1.2.3""#);
    assert_eq!(version.result().unwrap().unwrap(), "1.2.3");
}
