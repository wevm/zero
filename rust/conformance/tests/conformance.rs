//! Every replayed vector case matches JS. Files no suite replays yet are
//! listed in the report, not failed; `cargo run -p zero-conformance` prints
//! it.

#[test]
fn replayed_vectors_match_js() {
    // The mirror (wevm/zero-rust) has rust/ without the monorepo's vectors.
    // In the monorepo they're always there, and CI's conformance step fails
    // without them.
    if !zero_conformance::vectors_dir().exists() {
        eprintln!(
            "skipped: no vectors at {}; set ZERO_CONFORMANCE_VECTORS",
            zero_conformance::vectors_dir().display()
        );
        return;
    }
    let report = zero_conformance::run(None).expect("vectors");
    assert!(report.ok(), "{}", report.markdown());
}
