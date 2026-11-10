# Zero for Rust

A native Rust client for Zero, in progress. See [DESIGN.md](DESIGN.md) for
the plan, the milestones, and the documented differences from JS.

```sh
cd rust
cargo test --workspace            # unit tests, and every replayed vector matches JS
cargo run -p zero-conformance     # the conformance report, file by file
```

The conformance runner reads `packages/zero-conformance/vectors`, and the
seeded fuzz corpora in `packages/zero-conformance/generated` when they are
there (`pnpm --filter zero-conformance run test` writes them).
