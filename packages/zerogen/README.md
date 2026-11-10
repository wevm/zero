# zerogen

Generates native client code for a [Zero](https://github.com/wevm/zero) app
from its TypeScript schema. Your schema, queries, and mutators stay the source
of truth. Swift is the one target so far; Rust is planned.

```sh
npm install --save-dev zerogen
npx zerogen --help
```

It needs Node 22.18 or later, which loads TypeScript files directly. It also
needs the app's own `@rocicorp/zero`, 1.11 or later.

## `zerogen codegen`

Generates every target in `zerogen.json`. `--target swift` generates only
Swift, and works without a config file.

```sh
npx zerogen codegen                         # reads zerogen.json
npx zerogen codegen --target swift --check  # fails when the output is out of date
```

```json
{
  "schema": "../web/shared/schema.ts",
  "targets": {
    "swift": {
      "output": "Sources/App/Schema.swift",
      "schemaName": "app",
      "tables": {"issue": {"columns": {"shortID": {"type": "int"}}}}
    }
  }
}
```

`schema` (default `schema.ts`) and `export` (default `schema`) name the
TypeScript schema module and the export that holds it, for every target.
`targets.swift` takes the other keys of
[`zero-swift.json`](https://github.com/wevm/zero/tree/main/packages/zero-swift-cli#zero-swift-codegen),
and generates the same Swift as `zero-swift codegen`, byte for byte.

Each key has an option that overrides it, such as `--schema-file`, `--out`,
`--type-prefix`, and `--access`. `--config` picks the config file. Run
`npx zerogen codegen --help` for the full list.

## `zerogen drift record`

Runs the TypeScript queries and mutators on samples and records what they did,
for a native client's drift test to check its handwritten ports against. It
records the same file as `zero-swift drift record`; see
[its docs](https://github.com/wevm/zero/tree/main/packages/zero-swift-cli#zero-swift-drift-record)
for the samples module.

```sh
npx zerogen drift record --samples drift-samples.ts --out Tests/AppTests/drift-vectors.json
```

## `zero-swift`

[`zero-swift`](https://www.npmjs.com/package/zero-swift) runs an exact
`zerogen` version, with its own `zero-swift.json`, flags, and output. It is
supported: an app using it doesn't need to move. To move, put the Swift keys of
`zero-swift.json` under `targets.swift` in `zerogen.json`, and run
`zerogen codegen` instead of `zero-swift codegen`.

## Output and agents

The CLI is built on [incur](https://github.com/wevm/incur). Errors print a
code and a message and exit 1, with no stack trace. `--json` (or `--format`)
prints the result as JSON, for scripts and CI. `--llms` prints a manifest of
the commands for a coding agent, and `npx zerogen skills add` installs them as
an agent skill.
