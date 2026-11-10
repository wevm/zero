# zerogen

The code generator shared by the CLIs of Zero's native clients. It's private:
it isn't published, and each client's CLI bundles it at build time, so the
published CLI has no dependency on it. Today that's
[`zero-swift`](../zero-swift-cli/README.md); a Rust client's CLI would bundle
it the same way.

- `src/cli.ts`: the `codegen` and `drift record` commands. It loads the
  schema, writes the output or checks it with `--check`, and reports usage
  errors.
- `src/types.ts`: reads the TypeScript types of `json<T>()` and
  `enumeration<T>()` columns, for the generated code to type them.
- `src/drift.ts`: runs the app's TypeScript queries and mutators on samples
  and records what they did, for a client's drift test.
- `src/targets/swift.ts`: the generated Swift, its names and escaping, and
  the options that shape it.

The tests are in the packages that bundle it, such as
`pnpm --filter zero-swift test` and `pnpm --filter zero-swift smoke`.
