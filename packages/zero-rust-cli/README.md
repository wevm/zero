# zero-rust

The command-line tool for [Zero for Rust](https://github.com/wevm/zero-rust)
(the [`zero-client`](https://crates.io/crates/zero-client) crate). Your Zero
app's TypeScript schema stays the source of truth, and this tool generates
the Rust the client needs from it: a row struct per table with its
`Update` and `Key`, the schema, a typed query builder (`zql`), and typed
CRUD for mutators (`mutate`).

```sh
npm install --save-dev zero-rust
npx zero-rust codegen --schema-file shared/schema.ts --out src/schema.rs
```

It needs Node 22.18 or later, which loads TypeScript files directly, and the
app's own `@rocicorp/zero`, 1.11 or later.

## `zero-rust codegen`

Options override `zero-rust.json`, which is read when present:

```json
{
  "schema": "../web/shared/schema.ts",
  "output": "src/schema.rs",
  "tables": {
    "issue": {"columns": {"shortID": {"type": "int"}}}
  }
}
```

`number()` columns are `f64`; set a column's type to `int` to make it an
`i64`, which must then hold integers. Include the output as a module with
`#[rustfmt::skip] mod schema;`. `--check` fails, without writing, when the
output is out of date, for CI. Run `npx zero-rust codegen --help` for every
option.

## Output and agents

The CLI is built on [incur](https://github.com/wevm/incur). `--format json`
prints the result as JSON, for scripts and CI. `--llms` prints a manifest of
the commands for a coding agent, and `npx zero-rust skills add` installs
them as an agent skill.
