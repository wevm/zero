//! Every replayed vector case matches JS. Files no suite replays yet are
//! listed in the report, not failed; `cargo run -p zero-conformance` prints
//! it.

#[test]
fn replayed_vectors_match_js() {
    let report = zero_conformance::run(None).expect("vectors");
    assert!(report.ok(), "{}", report.markdown());
}
