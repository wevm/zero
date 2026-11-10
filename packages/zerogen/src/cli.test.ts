import {mkdtempSync, readFileSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {expect, test} from 'vitest';
import {createCli} from './cli.ts';

async function run(args: string[]) {
  let output = '';
  let code = 0;
  await createCli('zerogen', '0.0.0').serve([...args, '--json'], {
    stdout: s => (output += s),
    exit: c => (code = c),
  });
  return {code, result: JSON.parse(output)};
}

test('configured Swift generation, overrides, and non-writing freshness checks', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'zerogen-'));
  const output = join(dir, 'Schema.swift');
  const config = join(dir, 'zerogen.json');
  try {
    writeFileSync(
      config,
      JSON.stringify({
        schema: fileURLToPath(
          new URL(
            '../../zero-swift-cli/test/fixture-schema.ts',
            import.meta.url,
          ),
        ),
        targets: {
          swift: {
            output,
            typePrefix: 'Cfg',
            schemaName: 'configured',
            tables: {project: {columns: {position: {type: 'int'}}}},
          },
        },
      }),
    );
    expect(
      await run(['codegen', '--config', config, '--type-prefix', 'Cli']),
    ).toMatchObject({
      code: 0,
      result: {targets: {swift: {written: true}}},
    });
    const swift = readFileSync(output, 'utf8');
    expect(swift).toContain('public struct CliProject');
    expect(swift).toContain('static let configured');
    expect(swift).toContain('var position: Int');
    expect(swift).not.toContain('CfgProject');
    const args = [
      'codegen',
      '--target',
      'swift',
      '--config',
      config,
      '--type-prefix',
      'Cli',
    ];
    expect(await run([...args, '--check'])).toMatchObject({
      code: 0,
      result: {targets: {swift: {upToDate: true}}},
    });
    writeFileSync(output, 'stale');
    expect(await run([...args, '--check'])).toMatchObject({
      code: 1,
      result: {code: 'OUT_OF_DATE'},
    });
    expect(readFileSync(output, 'utf8')).toBe('stale');
    expect(await run(args)).toMatchObject({code: 0});
    expect(readFileSync(output, 'utf8')).toBe(swift);
  } finally {
    rmSync(dir, {recursive: true, force: true});
  }
});

test('explicit selection ignores other targets, but all-target mode rejects unimplemented targets before writing', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'zerogen-targets-'));
  const output = join(dir, 'Schema.swift');
  const config = join(dir, 'config.json');
  try {
    writeFileSync(
      config,
      JSON.stringify({
        schema: fileURLToPath(
          new URL(
            '../../zero-swift-cli/test/fixture-schema.ts',
            import.meta.url,
          ),
        ),
        targets: {swift: {output}, rust: {}},
      }),
    );
    expect(
      await run(['codegen', '--target', 'swift', '--config', config]),
    ).toMatchObject({code: 0});
    writeFileSync(output, 'untouched');
    expect(await run(['codegen', '--config', config])).toMatchObject({
      code: 1,
      result: {code: 'UNSUPPORTED_TARGET'},
    });
    expect(readFileSync(output, 'utf8')).toBe('untouched');
    expect(
      await run(['codegen', '--target', 'rust', '--config', config]),
    ).toMatchObject({code: 1, result: {code: 'UNSUPPORTED_TARGET'}});
    writeFileSync(config, '{}');
    expect(await run(['codegen', '--config', config])).toMatchObject({
      code: 1,
      result: {code: 'NO_TARGETS'},
    });
  } finally {
    rmSync(dir, {recursive: true, force: true});
  }
});
