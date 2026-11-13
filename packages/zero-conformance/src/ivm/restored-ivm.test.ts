import {afterAll, describe, expect, vi} from 'vitest';
import type {JSONValue} from '../../../shared/src/json.ts';
import type {Row} from '../../../zero-protocol/src/data.ts';
import type {Schema} from '../../../zero-types/src/schema.ts';
import {ChangeType} from '../../../zql/src/ivm/change-type.ts';
import type {
  FetchTest,
  PushTest,
} from '../../../zql/src/ivm/test/fetch-and-push-tests.ts';
import {emitVectors} from '../vectors.ts';
import {runQuery, schemaJSON, type Push, type QueryResult} from './harness.ts';

/**
 * The cases of the zql tests restored from rocicorp/mono 7ebeb2172 (see
 * tools/restored-tests/README.md) that drive a pipeline built from an AST
 * through `runPushTest` or `runFetchTest`. The restored tests run here
 * unchanged, with their own assertions; this records each case they build and
 * replays it through the harness, which records the view and its changes after
 * every push.
 */
const recorded = vi.hoisted(
  () => [] as {name: string; test: PushTest | FetchTest}[],
);

vi.mock(
  import('../../../zql/src/ivm/test/fetch-and-push-tests.ts'),
  async importOriginal => {
    const actual = await importOriginal();
    const record = (test: PushTest | FetchTest) => {
      recorded.push({name: expect.getState().currentTestName ?? '?', test});
    };
    return {
      ...actual,
      runPushTest: (t: PushTest) => (record(t), actual.runPushTest(t)),
      runFetchTest: (t: FetchTest) => (record(t), actual.runFetchTest(t)),
    };
  },
);

// Importing these registers their tests here, which is the point: they run
// with their own assertions while the mock above records their cases.
/* oxlint-disable no-restricted-imports */
const files: [string, () => Promise<unknown>][] = [
  [
    'correlated-predicate-pushdown.push.test.ts',
    () =>
      import('../../../zql/src/builder/correlated-predicate-pushdown.push.test.ts'),
  ],
  [
    'exists.flip.push.test.ts',
    () => import('../../../zql/src/ivm/exists.flip.push.test.ts'),
  ],
  [
    'exists.push.test.ts',
    () => import('../../../zql/src/ivm/exists.push.test.ts'),
  ],
  [
    'runaway-push.test.ts',
    () => import('../../../zql/src/ivm/runaway-push.test.ts'),
  ],
  [
    'take-gate.overlay.push.test.ts',
    () => import('../../../zql/src/ivm/take-gate.overlay.push.test.ts'),
  ],
  [
    'take.push-child.test.ts',
    () => import('../../../zql/src/ivm/take.push-child.test.ts'),
  ],
];
/* oxlint-enable no-restricted-imports */

for (const [file, load] of files) {
  await describe(file, async () => {
    await load();
  });
}

type Case = {
  name: string;
  schema: JSONValue;
  data: Record<string, Row[]>;
  ast: unknown;
  format: unknown;
  options?: {
    enableNotExists?: true | undefined;
    disableCorrelatedPredicatePushdown?: true | undefined;
  };
  pushes: Push[];
  result: QueryResult;
};

function toCase(name: string, t: PushTest | FetchTest): Case {
  const schema: Schema = {
    tables: Object.fromEntries(
      Object.entries(t.sources).map(([table, {columns, primaryKeys}]) => [
        table,
        {name: table, columns, primaryKey: primaryKeys},
      ]),
    ),
    relationships: {},
  } as unknown as Schema;
  const pushes: Push[] = ('pushes' in t ? t.pushes : []).map(
    ([table, [type, row, oldRow]]) =>
      type === ChangeType.EDIT
        ? {table, type: 'edit', row, oldRow: oldRow as Row}
        : {table, type: type === ChangeType.ADD ? 'add' : 'remove', row},
  );
  const options = {
    ...('enableNotExists' in t && t.enableNotExists
      ? {enableNotExists: true as const}
      : {}),
    ...('disableCorrelatedPredicatePushdown' in t &&
    t.disableCorrelatedPredicatePushdown
      ? {disableCorrelatedPredicatePushdown: true as const}
      : {}),
  };
  const data = Object.fromEntries(
    Object.entries(t.sourceContents).map(([k, v]) => [k, [...v]]),
  );
  return {
    name,
    schema: schemaJSON(schema),
    data,
    ast: t.ast,
    format: t.format,
    ...(Object.keys(options).length ? {options} : {}),
    pushes,
    result: runQuery(schema, data, t.ast, t.format, pushes, options),
  };
}

afterAll(() => {
  const seen = new Map<string, number>();
  const cases = recorded.map(({name, test}) => {
    const n = (seen.get(name) ?? 0) + 1;
    seen.set(name, n);
    return toCase(n === 1 ? name : `${name} (${n})`, test);
  });
  emitVectors(
    'restored-ivm',
    'ivm-ast',
    'The runPushTest and runFetchTest cases of zql tests restored from rocicorp/mono 7ebeb2172: sources, an AST and pushes, with the view and its changes after hydration and after each push. See src/ivm/restored-ivm.test.ts.',
    cases,
    undefined,
    {compact: true},
  );
});
