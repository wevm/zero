import {readFileSync, writeFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {expect, test} from 'vitest';
import {codegenFile} from './cli.ts';
import type {Config} from './generate.ts';

const repo = fileURLToPath(new URL('../../../', import.meta.url));
const update = process.env['UPDATE_GENERATED'] === '1';

/**
 * The Swift tests compile and run these generated files, so they are checked
 * in; this keeps them current with the generator and the schemas.
 */
const fixtures: {name: string; config: Config}[] = [
  {
    name: 'zql test schema',
    config: {
      schema: 'packages/zql/src/query/test/test-schemas.ts',
      output: 'swift/Tests/ZeroTests/Generated/TestSchema.swift',
      schemaName: 'test',
    },
  },
  {
    name: 'codegen fixture schema',
    config: {
      schema: 'packages/zero-swift-cli/test/fixture-schema.ts',
      output: 'swift/Tests/ZeroTests/Generated/FixtureSchema.swift',
      schemaName: 'fixture',
      zql: 'fixtureZQL',
      typePrefix: 'Fx',
      tables: {project: {columns: {position: {type: 'int'}}}},
    },
  },
];

for (const {name, config} of fixtures) {
  test(`generated Swift for the ${name} is current`, async () => {
    const swift = await codegenFile(config, repo);
    const path = repo + config.output;
    if (update) {
      writeFileSync(path, swift);
    }
    expect(swift).toBe(readFileSync(path, 'utf8'));
  });
}
