import {configDefaults, defineConfig} from 'vitest/config';
import config from '../shared/src/tool/vitest-config.ts';

const {define, esbuild} = config;

// The ZQL suite against a `TableSource` that makes each derived change durable
// in its backing database, the way the source has always worked. Paired with
// `vitest.config.deferred-writes.ts`, which runs the identical suite against a
// read-only derivation; the two must be indistinguishable to every operator
// above the source.
export default defineConfig({
  define,
  esbuild,
  test: {
    name: 'zqlite-zql-test-write-through',
    include: ['../zql/src/**/*.test.ts'],
    // Changed in wevm/zero: `source.test.ts > push` expects a duplicate add to
    // throw, which `TableSource` stopped checking in upstream b0c414b71 (see
    // tools/restored-tests/README.md). It still runs against `MemorySource` in
    // zql's own suite.
    exclude: [...configDefaults.exclude, '../zql/src/ivm/source.test.ts'],
    setupFiles: ['./src/setup.ts'],
    testTimeout: 20_000,
  },
});
