//! `connection().connect(auth)` (`connect({auth})` in
//! `packages/zero-client/src/client/connection.ts`): a token a client
//! without a user can't take is an error before anything happens, and
//! clearing the token really drops it.

use std::rc::Rc;
use std::sync::Arc;
use zero_client::client::{
    ClientErrorKind, ConnectAuth, ConnectionError, ConnectionStatus, Error, Zero, ZeroOptions,
};
use zero_client::json::{self, Value};
use zero_client::protocol::sec_protocols;
use zero_client::schema::Schema;
use zero_client::testing::{ManualScheduler, MockSocket, MockSocketFactory, RecordingLogSink};

struct Harness {
    zero: Zero,
    scheduler: Rc<ManualScheduler>,
    factory: Rc<MockSocketFactory>,
}

impl Harness {
    fn new(user_id: Option<&str>, auth: Option<&str>) -> Self {
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
            user_id: user_id.map(str::to_owned),
            auth: auth.map(str::to_owned),
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

    fn connected(&self) {
        self.socket().receive_text(r#"["connected",{"wsid":"w"}]"#);
        self.settle();
        assert_eq!(self.status(), ConnectionStatus::Connected);
    }

    fn unauthorized(&self) {
        self.socket().receive_text(
            r#"["error",{"kind":"Unauthorized","message":"no","origin":"zeroCache"}]"#,
        );
        self.settle();
        assert_eq!(self.status(), ConnectionStatus::NeedsAuth);
    }

    fn status(&self) -> ConnectionStatus {
        self.zero.connection().state().status()
    }

    fn update_auths(&self) -> Vec<String> {
        self.socket()
            .messages()
            .into_iter()
            .filter(|m| m.starts_with(r#"["updateAuth""#))
            .collect()
    }

    /// The `authToken` the last socket opened with.
    fn auth_token(&self) -> Option<String> {
        let protocol =
            sec_protocols::decode(self.socket().sec_protocol.as_deref().unwrap()).unwrap();
        protocol
            .get("authToken")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
}

fn rejection(r: Result<(), Error>) -> ConnectionError {
    match r {
        Err(Error::Connection(e)) => e,
        r => panic!("not a connection error: {r:?}"),
    }
}

#[test]
fn an_invalid_pair_is_an_error_and_clear_drops_the_token() {
    // A client without a user can't take a token: JS's `#setAuth` throws,
    // so `connect` rejects before it does anything.
    let h = Harness::new(None, None);
    h.connected();
    let expected = ConnectionError::client(
        ClientErrorKind::Internal,
        "ZeroOptions.userID is required when auth is set.",
    );
    assert_eq!(
        rejection(h.zero.connection().connect(ConnectAuth::Set("t".into()))),
        expected
    );
    h.settle();
    assert!(h.update_auths().is_empty(), "no updateAuth while connected");
    h.unauthorized();
    assert_eq!(
        rejection(h.zero.connection().connect(ConnectAuth::Set("t".into()))),
        expected
    );
    h.settle();
    assert_eq!(h.factory.sockets().len(), 1, "no reconnect");
    assert_eq!(h.status(), ConnectionStatus::NeedsAuth);
    // An empty token is no token, so it is valid.
    h.zero
        .connection()
        .connect(ConnectAuth::Set(String::new()))
        .unwrap();
    h.settle();
    assert_eq!(h.factory.sockets().len(), 2);
    assert_eq!(h.auth_token(), None);

    // `auth: null` clears the token: it sends no `updateAuth`, and the
    // next connection carries none.
    let h = Harness::new(Some("u"), Some("t1"));
    assert_eq!(h.auth_token().as_deref(), Some("t1"));
    h.connected();
    h.zero
        .connection()
        .connect(ConnectAuth::Set("t2".into()))
        .unwrap();
    h.zero.connection().connect(ConnectAuth::Clear).unwrap();
    h.settle();
    assert_eq!(h.update_auths(), [r#"["updateAuth",{"auth":"t2"}]"#]);
    h.unauthorized();
    h.zero.connection().connect(ConnectAuth::Keep).unwrap();
    h.settle();
    assert_eq!(h.factory.sockets().len(), 2);
    assert_eq!(h.auth_token(), None);
}
