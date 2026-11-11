import {writeFileSync, readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {expect, test} from 'vitest';
import * as sweep from '../../zero-swift-cli/test/sweep-schema.ts';
import type {Config} from '../../zerogen/src/rust-cli.ts';
import {cli, codegenRustFile} from './cli.ts';

const repo = fileURLToPath(new URL('../../../', import.meta.url));
const update = process.env['UPDATE_GENERATED'] === '1';
const generated = 'rust/codegen-tests/src/';

/**
 * The Rust tests (rust/codegen-tests) compile these generated files, run
 * their queries, and read views as their rows, so they are checked in;
 * this keeps them current with the generator and the schemas. The sweep
 * schemas are the names the Swift generator has to work around, which
 * Rust has to as well.
 */
const fixtures: {name: string; config: Config}[] = [
  {
    name: 'zql test schema',
    config: {
      schema: 'packages/zql/src/query/test/test-schemas.ts',
      output: generated + 'test_schema.rs',
    },
  },
  {
    name: 'codegen fixture schema',
    config: {
      schema: 'packages/zero-swift-cli/test/fixture-schema.ts',
      output: generated + 'fixture.rs',
      typePrefix: 'Fx',
      tables: {project: {columns: {position: {type: 'int'}}}},
    },
  },
  {
    name: 'zbugs schema',
    config: {
      schema: 'apps/zbugs/shared/schema.ts',
      output: generated + 'zbugs.rs',
      tables: {
        issue: {columns: {shortID: {type: 'int'}}},
        project: {columns: {issueCountEstimate: {type: 'int'}}},
      },
    },
  },
  ...Object.keys(sweep).map(name => ({
    name: `sweep schema ${name}`,
    config: {
      schema: 'packages/zero-swift-cli/test/sweep-schema.ts',
      export: name,
      output: `${generated}sweep/${snake(name)}.rs`,
    },
  })),
];

function snake(name: string) {
  return name.replace(/([a-z0-9])([A-Z])/g, '$1_$2').toLowerCase();
}

for (const {name, config} of fixtures) {
  test(`generated Rust for the ${name} is current`, async () => {
    const rust = await codegenRustFile(config, repo);
    const path = repo + config.output;
    if (update) {
      writeFileSync(path, rust);
    }
    expect(rust).toBe(readFileSync(path, 'utf8'));
  });
}

test('the sweep module lists every sweep schema', () => {
  const mod = [
    '// The generated Rust of each schema of',
    '// packages/zero-swift-cli/test/sweep-schema.ts, which only has to compile.',
    ...Object.keys(sweep).map(n => `#[rustfmt::skip]\npub mod ${snake(n)};`),
    '',
  ].join('\n');
  const path = repo + generated + 'sweep/mod.rs';
  if (update) {
    writeFileSync(path, mod);
  }
  expect(mod).toBe(readFileSync(path, 'utf8'));
});

test('a missing schema is a usage error, not a crash', async () => {
  let out = '';
  let code: number | undefined;
  await cli.serve(['codegen', '--schema-file', 'no-such-schema.ts', '--json'], {
    stdout: s => (out += s),
    exit: c => (code = c),
  });
  expect(code).toBe(1);
  expect(JSON.parse(out)).toMatchObject({
    code: 'SCHEMA_NOT_FOUND',
    message: `Schema no-such-schema.ts doesn't exist. Pass --schema-file, or set "schema" in zero-rust.json.`,
  });
});
