/**
 * Installs the packed zero-rust in a project outside the workspace, with the
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
const dir = mkdtempSync(join(tmpdir(), 'zero-rust-packed-'));
const installed = join(dir, 'node_modules/zero-rust');
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
  command('pnpm', ['--filter', 'zero-rust', 'build']);
  command('pnpm', ['--filter', 'zero-rust', 'pack', '--pack-destination', dir]);
  writeFileSync(
    join(dir, 'package.json'),
    JSON.stringify({
      private: true,
      type: 'module',
      dependencies: {
        'zero-rust': `file:./zero-rust-${pkg.version}.tgz`,
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
    join(dir, 'zero-rust.json'),
    JSON.stringify({
      schema: 'schema.ts',
      output: 'schema.rs',
      typePrefix: 'Fx',
      tables: {issue: {columns: {position: {type: 'int'}}}},
    }),
  );
  assert.equal(run(['codegen']).written, true);
  const rust = readFileSync(join(dir, 'schema.rs'), 'utf8');
  assert.match(rust, /pub struct FxIssue/);
  assert.match(rust, /pub position: i64/);
  assert.match(rust, /pub enum FxStatus/);
  assert.match(rust, /pub struct FxDetails/);
  assert.equal(run(['codegen']).written, false);
  assert.equal(run(['codegen', '--check']).upToDate, true);
  writeFileSync(join(dir, 'schema.rs'), 'stale');
  assert.equal(run(['codegen', '--check'], 1).code, 'OUT_OF_DATE');
  assert.equal(readFileSync(join(dir, 'schema.rs'), 'utf8'), 'stale');
  run(['codegen']);
  assert.equal(readFileSync(join(dir, 'schema.rs'), 'utf8'), rust);
  process.stdout.write(
    'Packed zero-rust: no zerogen dependency, codegen, and non-writing --check passed.\n',
  );
} finally {
  rmSync(dir, {recursive: true, force: true});
}
