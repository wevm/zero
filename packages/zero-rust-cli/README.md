# zero-rust

Generates the Rust row types, schema, and typed query builder of a Zero app
from its TypeScript schema, for the Zero Rust client (the `zero-client`
crate, [`rust/`](../../rust)).

```sh
npx zero-rust codegen --schema-file shared/schema.ts --out src/schema.rs
```

Options override `zero-rust.json`, which is read when present. In CI,
`zero-rust codegen --check` fails when the output is stale. The generator is
`packages/zerogen`, shared with `zero-swift` and bundled in.

This package also carries the Rust client's version for Changesets: its
version is the crates' version, and a `zero-rust` changeset releases both
(see `.changeset/README.md`). `pnpm --filter zero-rust sync-version` copies
it into `rust/Cargo.toml` and `rust/Cargo.lock`.
