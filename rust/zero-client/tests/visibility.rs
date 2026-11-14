//! `ZeroOptions::default()` waits JS's 5 minutes (`hiddenTabDisconnectDelay`)
//! after `set_visible(false)` before it disconnects. The conformance vectors
//! set the delay themselves, so they don't check the default.

use std::rc::Rc;
use std::sync::Arc;
use zero_client::client::{ConnectionState, Zero, ZeroOptions};
use zero_client::json;
use zero_client::schema::Schema;
use zero_client::testing::{ManualScheduler, MockSocketFactory, RecordingLogSink};

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
