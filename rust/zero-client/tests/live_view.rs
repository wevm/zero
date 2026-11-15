//! `Zero::live`: a view read as row structs and kept current, against a
//! mock server.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use zero_client::ast::System;
use zero_client::client::{LiveError, LiveView, Zero, ZeroOptions};
use zero_client::hash::query::hash_of_name_and_args;
use zero_client::ivm::{ResultType, ViewEntry, ViewValue};
use zero_client::json;
use zero_client::query::AnyQuery;
use zero_client::schema::Schema;
use zero_client::testing::{ManualScheduler, MockSocket, MockSocketFactory, RecordingLogSink};
use zero_client::ttl::Ttl;
use zero_client::typed::{
    OptionalQueryElement, Query, QueryElement, ZeroValueError, column, optional_row,
};

#[derive(Debug, PartialEq)]
struct Issue {
    id: String,
    title: String,
}

thread_local! {
    /// How many issues were read from entries.
    static READS: Cell<usize> = const { Cell::new(0) };
}

fn reads() -> usize {
    READS.with(Cell::get)
}

impl QueryElement for Issue {
    fn from_entry(entry: &ViewEntry) -> Result<Self, ZeroValueError> {
        READS.with(|r| r.set(r.get() + 1));
        let title: String = column(entry, "title", 1)?;
        // A title a newer server might send, which this app can't read.
        if title == "unreadable" {
            return Err(ZeroValueError("an unreadable title".into()));
        }
        Ok(Issue {
            id: column(entry, "id", 0)?,
            title,
        })
    }
}

impl OptionalQueryElement for Issue {
    fn optional_from_view(view: &ViewValue) -> Result<Option<Self>, ZeroValueError> {
        optional_row(view)
    }
}

fn issue(id: &str, title: &str) -> String {
    format!(r#"{{"op":"put","tableName":"issue","value":{{"id":"{id}","title":"{title}"}}}}"#)
}

fn del(id: &str) -> String {
    format!(r#"{{"op":"del","tableName":"issue","id":{{"id":"{id}"}}}}"#)
}

struct Harness {
    zero: Zero,
    scheduler: Rc<ManualScheduler>,
    factory: Rc<MockSocketFactory>,
    schema: Arc<Schema>,
    cookie: Cell<u32>,
}

impl Harness {
    fn new() -> Self {
        let schema = Arc::new(
            Schema::from_json(
                &json::parse(
                    r#"{"tables": {"issue": {"name": "issue", "primaryKey": ["id"],
                        "columns": {"id": {"type": "string"}, "title": {"type": "string"}}}}}"#,
                )
                .unwrap(),
            )
            .unwrap(),
        );
        let scheduler = ManualScheduler::new(1_700_000_000_000.0);
        let factory = MockSocketFactory::new();
        let options = ZeroOptions {
            cache_url: Some("http://zero.test".into()),
            user_id: Some("u1".into()),
            log_sink: Some(Rc::new(RecordingLogSink::default())),
            ..ZeroOptions::default()
        };
        let zero = Zero::new(
            schema.clone(),
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
            schema,
            cookie: Cell::new(0),
        };
        h.connect();
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
        self.settle();
        self.socket().receive_text(r#"["connected",{"wsid":"w1"}]"#);
        self.settle();
    }

    /// Every issue, by `id`.
    fn issues(&self) -> Query<Issue> {
        Query::from_any(
            AnyQuery::new(self.schema.clone(), "issue", System::Client)
                .order_by("id", zero_client::ast::Direction::Asc),
        )
    }

    /// Every issue, as the named query `issues`.
    fn named_issues(&self) -> Query<Issue> {
        Query::from_any(self.issues().into_any().name_and_args("issues", vec![]))
    }

    fn live(&self, query: &Query<Issue>) -> LiveView<Vec<Issue>> {
        self.zero.live(query, &Ttl::default()).unwrap()
    }

    /// A poke from the server: `rows` (row patch ops), and the queries it
    /// has now got.
    fn poke(&self, rows: &[String], got: &[&str]) {
        let base = match self.cookie.get() {
            0 => "null".to_owned(),
            c => format!("\"{c:04}\""),
        };
        let cookie = self.cookie.get() + 1;
        self.cookie.set(cookie);
        let got: Vec<String> = got
            .iter()
            .map(|h| format!(r#"{{"op":"put","hash":"{h}"}}"#))
            .collect();
        let socket = self.socket();
        socket.receive_text(&format!(
            r#"["pokeStart",{{"pokeID":"p{cookie}","baseCookie":{base}}}]"#
        ));
        socket.receive_text(&format!(
            r#"["pokePart",{{"pokeID":"p{cookie}","rowsPatch":[{}],"gotQueriesPatch":[{}]}}]"#,
            rows.join(","),
            got.join(",")
        ));
        socket.receive_text(&format!(
            r#"["pokeEnd",{{"pokeID":"p{cookie}","cookie":"{cookie:04}"}}]"#
        ));
        self.settle();
    }
}

fn titles(live: &LiveView<Vec<Issue>>) -> Vec<String> {
    live.value().iter().map(|i| i.title.clone()).collect()
}

#[test]
fn the_value_follows_pokes() {
    let h = Harness::new();
    let live = h.live(&h.issues());
    assert!(live.value().is_empty());
    assert_eq!(live.result_type(), ResultType::Unknown);
    h.poke(&[issue("a", "Apple"), issue("b", "Banana")], &[]);
    assert_eq!(
        *live.value(),
        vec![
            Issue {
                id: "a".into(),
                title: "Apple".into()
            },
            Issue {
                id: "b".into(),
                title: "Banana".into()
            },
        ]
    );
    assert_eq!(live.error(), None);
}

#[test]
fn a_listener_hears_the_value_now_and_on_every_update() {
    let h = Harness::new();
    let live = h.live(&h.issues());
    let heard: Rc<RefCell<Vec<Vec<String>>>> = Rc::default();
    let h2 = heard.clone();
    let remove = live.add_listener(move |issues, _| {
        h2.borrow_mut()
            .push(issues.iter().map(|i| i.title.clone()).collect());
    });
    h.poke(&[issue("a", "Apple")], &[]);
    h.poke(&[issue("a", "Apricot")], &[]);
    assert_eq!(
        *heard.borrow(),
        vec![vec![], vec!["Apple".to_owned()], vec!["Apricot".to_owned()]]
    );
    remove();
    h.poke(&[issue("b", "Banana")], &[]);
    assert_eq!(heard.borrow().len(), 3, "removed");
    assert_eq!(titles(&live), ["Apricot", "Banana"]);
}

#[test]
fn the_result_type_goes_complete_once_the_server_has_the_query() {
    let h = Harness::new();
    let live = h.live(&h.named_issues());
    let types: Rc<RefCell<Vec<ResultType>>> = Rc::default();
    let t = types.clone();
    let _remove = live.add_listener(move |_, ty| t.borrow_mut().push(ty));
    assert_eq!(live.result_type(), ResultType::Unknown);
    h.poke(
        &[issue("a", "Apple")],
        &[&hash_of_name_and_args("issues", &[])],
    );
    assert_eq!(live.result_type(), ResultType::Complete);
    assert_eq!(titles(&live), ["Apple"]);
    assert_eq!(types.borrow().last(), Some(&ResultType::Complete));
}

#[test]
fn a_query_the_server_could_not_run_is_an_error() {
    let h = Harness::new();
    let live = h.live(&h.named_issues());
    let hash = hash_of_name_and_args("issues", &[]);
    h.socket().receive_text(&format!(
        r#"["transformError",[{{"error":"app","id":"{hash}","name":"issues","message":"boom"}}]]"#
    ));
    h.settle();
    assert_eq!(live.result_type(), ResultType::Error);
    let error = live.error().expect("an error");
    assert!(matches!(error, LiveError::Query(_)), "{error:?}");
    assert_eq!(error.to_string(), "boom");
}

#[test]
fn rows_that_do_not_read_keep_the_last_value_and_set_the_error() {
    let h = Harness::new();
    let live = h.live(&h.issues());
    let heard = Rc::new(Cell::new(0));
    let n = heard.clone();
    let _remove = live.add_listener(move |_, _| n.set(n.get() + 1));
    h.poke(&[issue("a", "Apple")], &[]);
    h.poke(&[issue("b", "unreadable")], &[]);
    assert_eq!(titles(&live), ["Apple"], "the last value read");
    assert_eq!(
        live.error(),
        Some(LiveError::Decode(ZeroValueError(
            "an unreadable title".into()
        )))
    );
    assert_eq!(heard.get(), 3, "the listener heard the failed read");
    // Rows that read again clear it.
    h.poke(&[issue("b", "Banana")], &[]);
    assert_eq!(titles(&live), ["Apple", "Banana"]);
    assert_eq!(live.error(), None);
}

#[test]
fn rows_that_do_not_read_at_first_fail_live() {
    let h = Harness::new();
    h.poke(&[issue("a", "unreadable")], &[]);
    let error = h.zero.live(&h.issues(), &Ttl::default()).err();
    assert_eq!(
        error.map(|e| e.message()),
        Some("an unreadable title".to_owned())
    );
}

#[test]
fn only_the_rows_that_changed_are_read_again() {
    let h = Harness::new();
    h.poke(
        &["a", "b", "c", "d", "e"].map(|id| issue(id, &id.to_uppercase())),
        &[],
    );
    let live = h.live(&h.issues());
    assert_eq!(titles(&live), ["A", "B", "C", "D", "E"]);
    let mut before = reads();

    let mut read = |live: &LiveView<Vec<Issue>>, titles_now: &[&str]| {
        assert_eq!(titles(live), titles_now);
        let n = reads() - before;
        before = reads();
        n
    };
    h.poke(&[issue("c", "C2")], &[]);
    assert_eq!(read(&live, &["A", "B", "C2", "D", "E"]), 1, "a change");
    h.poke(&[issue("bb", "BB")], &[]);
    assert_eq!(
        read(&live, &["A", "B", "BB", "C2", "D", "E"]),
        1,
        "an insert"
    );
    h.poke(&[del("a")], &[]);
    assert_eq!(read(&live, &["B", "BB", "C2", "D", "E"]), 0, "a removal");
    h.poke(&[del("c"), issue("f", "F")], &[]);
    assert_eq!(read(&live, &["B", "BB", "D", "E", "F"]), 1, "both");
    h.poke(&[issue("b", "B2"), issue("e", "E2")], &[]);
    assert_eq!(read(&live, &["B2", "BB", "D", "E2", "F"]), 2, "two changes");

    // A value the app still holds is left alone: the update reads the rows
    // anew.
    let held = live.value();
    h.poke(&[issue("d", "D2")], &[]);
    assert_eq!(read(&live, &["B2", "BB", "D2", "E2", "F"]), 5, "held");
    assert_eq!(held[2].title, "D");
}

#[test]
fn the_view_moves_to_the_client_that_replaces_this_one() {
    let h = Harness::new();
    let live = h.live(&h.issues());
    let heard: Rc<RefCell<Vec<Vec<String>>>> = Rc::default();
    let h2 = heard.clone();
    let _remove = live.add_listener(move |issues, _| {
        h2.borrow_mut()
            .push(issues.iter().map(|i| i.title.clone()).collect());
    });
    h.poke(&[issue("a", "Apple")], &[]);
    let first = h.zero.client_id();
    let sockets = h.factory.sockets().len();

    // The server lost the client's state: the client replaces itself.
    h.socket().receive_text(
        r#"["error",{"kind":"ClientNotFound","message":"gone","origin":"zeroCache"}]"#,
    );
    h.settle();
    assert_ne!(h.zero.client_id(), first, "replaced");
    assert!(h.factory.sockets().len() > sockets, "a new connection");
    h.cookie.set(0);
    h.connect();
    h.poke(&[issue("b", "Banana")], &[]);
    assert_eq!(titles(&live), ["Banana"]);
    // The new client's view, of its own client group's rows, and then its
    // first poke.
    assert_eq!(
        *heard.borrow(),
        [
            vec![],
            vec!["Apple".to_owned()],
            vec![],
            vec!["Banana".to_owned()]
        ]
    );
    assert!(!live.view().is_destroyed());
}

#[test]
fn dropping_it_removes_its_listeners_and_destroys_the_view() {
    let h = Harness::new();
    let live = h.live(&h.issues());
    let heard = Rc::new(Cell::new(0));
    let n = heard.clone();
    let _remove = live.add_listener(move |_, _| n.set(n.get() + 1));
    let view = live.view().clone();
    assert_eq!(Rc::strong_count(&heard), 2);
    drop(live);
    assert_eq!(Rc::strong_count(&heard), 1, "the listener is gone");
    assert!(view.is_destroyed());
    h.poke(&[issue("a", "Apple")], &[]);
    assert_eq!(heard.get(), 1);
}

#[test]
fn a_one_query_reads_its_row() {
    let h = Harness::new();
    let first: Query<Issue, Option<Issue>> = Query::from_any(h.issues().into_any().one());
    let first = h.zero.live(&first, &Ttl::default()).unwrap();
    assert_eq!(*first.value(), None);
    h.poke(&[issue("a", "Apple"), issue("b", "Banana")], &[]);
    assert_eq!(
        first.value().as_ref().as_ref().map(|i| i.id.as_str()),
        Some("a")
    );
}

#[cfg(feature = "serde")]
#[test]
fn a_named_query_request_reads_as_its_rows() {
    use zero_client::define::{QueryDefinition, define_query};
    thread_local! {
        static SCHEMA: RefCell<Option<Arc<Schema>>> = const { RefCell::new(None) };
    }
    const ISSUES: QueryDefinition<(), (), Issue> = define_query("issues", |_, _| {
        let schema = SCHEMA.with(|s| s.borrow().clone()).unwrap();
        Query::from_any(AnyQuery::new(schema, "issue", System::Client))
    });
    let h = Harness::new();
    SCHEMA.with(|s| *s.borrow_mut() = Some(h.schema.clone()));
    let live = h.zero.live(&ISSUES.with(()), &Ttl::default()).unwrap();
    h.poke(
        &[issue("a", "Apple")],
        &[&hash_of_name_and_args("issues", &[])],
    );
    assert_eq!(titles(&live), ["Apple"]);
    assert_eq!(live.result_type(), ResultType::Complete);
}
