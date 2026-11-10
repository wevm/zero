/**
 * Installs the packed zero-swift in a project outside the workspace, with the
 * app's own @rocicorp/zero, and runs it. zerogen is bundled in, so the package
 * must not depend on it.
 */
import assert from 'node:assert/strict';
import {execFileSync, spawnSync} from 'node:child_process';
import {
  existsSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {fileURLToPath} from 'node:url';
import pkg from '../package.json' with {type: 'json'};

const repo = fileURLToPath(new URL('../../../', import.meta.url));
const dir = mkdtempSync(join(tmpdir(), 'zero-swift-packed-'));
const installed = join(dir, 'node_modules/zero-swift');
function command(bin: string, args: string[], cwd = repo) {
  return execFileSync(bin, args, {
    cwd,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  });
}
function run(args: string[], status = 0) {
  const result = spawnSync(
    process.execPath,
    [join(installed, 'dist/bin.js'), ...args, '--json'],
    {cwd: dir, encoding: 'utf8'},
  );
  assert.equal(result.status, status, result.stdout + result.stderr);
  return JSON.parse(result.stdout);
}

try {
  command('pnpm', ['--filter', 'zero-swift', 'build']);
  command('pnpm', [
    '--filter',
    'zero-swift',
    'pack',
    '--pack-destination',
    dir,
  ]);
  writeFileSync(
    join(dir, 'package.json'),
    JSON.stringify({
      private: true,
      type: 'module',
      dependencies: {
        'zero-swift': `file:./zero-swift-${pkg.version}.tgz`,
        '@rocicorp/zero': '1.11.0',
      },
    }),
  );
  // npm, in a throwaway project outside the workspace, so only the tarball's
  // own dependencies resolve and the repository's lockfile isn't touched.
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

  const manifest = JSON.parse(
    readFileSync(join(installed, 'package.json'), 'utf8'),
  );
  assert.deepEqual(manifest.dependencies, pkg.dependencies);
  for (const field of [
    'dependencies',
    'peerDependencies',
    'optionalDependencies',
  ]) {
    assert.equal(manifest[field]?.zerogen, undefined, field);
  }
  assert.equal(existsSync(join(dir, 'node_modules/zerogen')), false);
  for (const file of readdirSync(join(installed, 'dist'))) {
    const code = readFileSync(join(installed, 'dist', file), 'utf8');
    assert.doesNotMatch(code, /(from|import\()\s*["']zerogen/, file);
  }
  assert.equal(
    command(
      process.execPath,
      [join(installed, 'dist/bin.js'), '--version'],
      dir,
    ).trim(),
    pkg.version,
  );

  writeFileSync(
    join(dir, 'schema.ts'),
    `
import {createSchema, table, string, number, enumeration, json} from '@rocicorp/zero';
type Status = 'open' | 'closed';
type Details = {label: string; tags: string[]};
export const schema = createSchema({tables: [table('issue').columns({id: string(), status: enumeration<Status>(), position: number(), details: json<Details>()}).primaryKey('id')]});
`,
  );
  writeFileSync(
    join(dir, 'zero-swift.json'),
    JSON.stringify({
      schema: 'schema.ts',
      output: 'Schema.swift',
      typePrefix: 'Fx',
      tables: {issue: {columns: {position: {type: 'int'}}}},
    }),
  );
  assert.equal(run(['codegen']).written, true);
  const swift = readFileSync(join(dir, 'Schema.swift'), 'utf8');
  assert.match(swift, /public struct FxIssue/);
  assert.match(swift, /var position: Int/);
  assert.match(swift, /enum FxStatus/);
  assert.match(swift, /struct FxDetails/);
  assert.equal(run(['codegen']).written, false);
  assert.equal(
    run([
      'codegen',
      '--schema=schema.ts',
      '--typePrefix=Fx',
      '--schemaName=app',
      '--check',
    ]).upToDate,
    true,
  );
  writeFileSync(join(dir, 'Schema.swift'), 'stale');
  assert.equal(run(['codegen', '--check'], 1).code, 'OUT_OF_DATE');
  assert.equal(readFileSync(join(dir, 'Schema.swift'), 'utf8'), 'stale');
  run(['codegen']);
  assert.equal(readFileSync(join(dir, 'Schema.swift'), 'utf8'), swift);

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
  assert.equal(run(['drift', 'record']).written, true);
  const vectors = JSON.parse(
    readFileSync(join(dir, 'drift-vectors.json'), 'utf8'),
  );
  assert.deepEqual(vectors.queries, ['all']);
  assert.deepEqual(vectors.mutators, ['add']);
  assert.equal(vectors.querySamples[0].ast.table, 'issue');
  assert.deepEqual(vectors.mutatorSamples[0].changes, [
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
  assert.equal(run(['drift', 'record', '--check']).upToDate, true);
  writeFileSync(join(dir, 'drift-vectors.json'), 'stale');
  assert.equal(run(['drift', 'record', '--check'], 1).code, 'OUT_OF_DATE');
  assert.equal(readFileSync(join(dir, 'drift-vectors.json'), 'utf8'), 'stale');
  process.stdout.write(
    'Packed zero-swift: no zerogen dependency, codegen, legacy flags, drift recording, and non-writing --check passed.\n',
  );
} finally {
  rmSync(dir, {recursive: true, force: true});
}
