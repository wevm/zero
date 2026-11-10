/** Install only the tarballs in an isolated consumer, never workspace links. */
import assert from 'node:assert/strict';
import {execFileSync, spawnSync} from 'node:child_process';
import {mkdtempSync, readFileSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {fileURLToPath} from 'node:url';
import wrapper from '../../zero-swift-cli/package.json' with {type: 'json'};
import generator from '../package.json' with {type: 'json'};

const repo = fileURLToPath(new URL('../../../', import.meta.url));
const dir = mkdtempSync(join(tmpdir(), 'zerogen-packed-'));
function command(bin: string, args: string[], cwd = repo) {
  return execFileSync(bin, args, {
    cwd,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  });
}
function run(bin: string, args: string[], status = 0) {
  const result = spawnSync(
    process.execPath,
    [join(dir, 'node_modules', bin, 'dist/bin.js'), ...args, '--json'],
    {cwd: dir, encoding: 'utf8'},
  );
  assert.equal(result.status, status, result.stdout + result.stderr);
  return JSON.parse(result.stdout);
}

try {
  for (const name of ['zerogen', 'zero-swift']) {
    command('pnpm', ['--filter', name, 'build']);
    command('pnpm', ['--filter', name, 'pack', '--pack-destination', dir]);
  }
  writeFileSync(
    join(dir, 'package.json'),
    JSON.stringify({
      private: true,
      type: 'module',
      dependencies: {
        'zerogen': `file:./zerogen-${generator.version}.tgz`,
        'zero-swift': `file:./zero-swift-${wrapper.version}.tgz`,
        '@rocicorp/zero': '1.11.0',
      },
    }),
  );
  // npm is used only in this disposable, external consumer. It cannot alter
  // the repository's pnpm lockfile and does not need an unpublished zerogen.
  command(
    'npm',
    [
      'install',
      '--ignore-scripts',
      '--no-audit',
      '--no-fund',
      '--package-lock=false',
    ],
    dir,
  );
  const installed = JSON.parse(
    readFileSync(join(dir, 'node_modules/zero-swift/package.json'), 'utf8'),
  );
  assert.equal(installed.dependencies.zerogen, generator.version);
  for (const [bin, version] of [
    ['zerogen', generator.version],
    ['zero-swift', wrapper.version],
  ]) {
    assert.equal(
      command(
        process.execPath,
        [join(dir, `node_modules/${bin}/dist/bin.js`), '--version'],
        dir,
      ).trim(),
      version,
    );
  }
  writeFileSync(
    join(dir, 'schema.ts'),
    `
import {createSchema, table, string, number, enumeration, json} from '@rocicorp/zero';
type Status = 'open' | 'closed';
type Details = {label: string; tags: string[]};
export const schema = createSchema({tables: [table('issue').columns({id: string(), status: enumeration<Status>(), position: number(), details: json<Details>()}).primaryKey('id')]});
`,
  );
  const swift = {
    output: 'Schema.swift',
    typePrefix: 'Fx',
    tables: {issue: {columns: {position: {type: 'int'}}}},
  };
  writeFileSync(
    join(dir, 'zerogen.json'),
    JSON.stringify({schema: 'schema.ts', targets: {swift}}),
  );
  writeFileSync(
    join(dir, 'zero-swift.json'),
    JSON.stringify({schema: 'schema.ts', ...swift}),
  );
  assert.equal(run('zerogen', ['codegen']).targets.swift.written, true);
  const contents = readFileSync(join(dir, 'Schema.swift'), 'utf8');
  assert.match(contents, /public struct FxIssue/);
  assert.match(contents, /var position: Int/);
  assert.match(contents, /enum FxStatus/);
  assert.match(contents, /struct FxDetails/);
  assert.equal(run('zero-swift', ['codegen']).written, false);
  assert.equal(readFileSync(join(dir, 'Schema.swift'), 'utf8'), contents);
  assert.equal(
    run('zero-swift', [
      'codegen',
      '--schema=schema.ts',
      '--typePrefix=Fx',
      '--schemaName=app',
      '--check',
    ]).upToDate,
    true,
  );
  assert.equal(
    run('zerogen', ['codegen', '--target', 'swift', '--check']).targets.swift
      .upToDate,
    true,
  );
  writeFileSync(join(dir, 'Schema.swift'), 'stale');
  for (const bin of ['zerogen', 'zero-swift']) {
    assert.equal(run(bin, ['codegen', '--check'], 1).code, 'OUT_OF_DATE');
    assert.equal(readFileSync(join(dir, 'Schema.swift'), 'utf8'), 'stale');
  }
  run('zerogen', ['codegen']);
  assert.equal(readFileSync(join(dir, 'Schema.swift'), 'utf8'), contents);
  assert.equal(
    run('zerogen', ['codegen', '--target', 'rust'], 1).code,
    'UNSUPPORTED_TARGET',
  );

  writeFileSync(
    join(dir, 'drift-samples.ts'),
    `
import {createBuilder, defineQuery, defineQueries, defineMutator, defineMutators} from '@rocicorp/zero';
import {schema} from './schema.ts';
const zql = createBuilder(schema);
const queries = defineQueries({all: defineQuery(() => zql.issue)});
const mutators = defineMutators({add: defineMutator(async ({tx, args}) => { await tx.mutate.issue.insert(args); })});
export default {schema, queries, mutators, querySamples: [{name: 'all'}], mutatorSamples: [{name: 'add', args: {id: 'a', status: 'open', position: 7, details: {label: 'test', tags: []}}}]};
`,
  );
  assert.equal(run('zerogen', ['drift', 'record']).written, true);
  const vectors = readFileSync(join(dir, 'drift-vectors.json'), 'utf8');
  const data = JSON.parse(vectors);
  assert.deepEqual(data.queries, ['all']);
  assert.deepEqual(data.mutators, ['add']);
  assert.equal(data.querySamples[0].ast.table, 'issue');
  assert.deepEqual(data.mutatorSamples[0].changes, [
    {
      table: 'issue',
      op: 'put',
      row: {
        id: 'a',
        status: 'open',
        position: 7,
        details: {label: 'test', tags: []},
      },
    },
  ]);
  assert.equal(run('zero-swift', ['drift', 'record']).written, false);
  assert.equal(readFileSync(join(dir, 'drift-vectors.json'), 'utf8'), vectors);
  for (const bin of ['zerogen', 'zero-swift']) {
    assert.equal(run(bin, ['drift', 'record', '--check']).upToDate, true);
    writeFileSync(join(dir, 'drift-vectors.json'), 'stale');
    assert.equal(
      run(bin, ['drift', 'record', '--check'], 1).code,
      'OUT_OF_DATE',
    );
    assert.equal(
      readFileSync(join(dir, 'drift-vectors.json'), 'utf8'),
      'stale',
    );
    writeFileSync(join(dir, 'drift-vectors.json'), vectors);
  }
  process.stdout.write(
    'Packed zerogen and zero-swift: versions, exact dependency, Swift parity, legacy flags, drift recording, and non-writing --check passed.\n',
  );
} finally {
  rmSync(dir, {recursive: true, force: true});
}
