# zero-client

A native Rust client for [Zero](https://zero.rocicorp.dev), in progress.

It is a port of the JS client, checked case by case against the JS
implementation with the conformance vectors in
[`packages/zero-conformance`](https://github.com/wevm/zero/tree/main/packages/zero-conformance).
So far it has Zero's primitives: JSON with JS semantics, JS number formatting
and parsing, string orders, hashes, row keys, value comparison, `LIKE`, and
TTLs. Syncing, queries, and mutators come next.

- [Design and milestones](https://github.com/wevm/zero/blob/main/rust/DESIGN.md)
- [Parity with JS](https://github.com/wevm/zero/blob/main/rust/PARITY.md): what
  is ported, and the documented differences

Not released yet.
