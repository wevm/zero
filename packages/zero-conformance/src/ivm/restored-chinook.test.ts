import {test} from 'vitest';
import {pkOf} from '../../../zql-integration-tests/src/chinook/fuzz/axes.ts';
import {
  applyLimit,
  applyOrder,
} from '../../../zql-integration-tests/src/chinook/fuzz/cover.ts';
import {flipVariants} from '../../../zql-integration-tests/src/chinook/fuzz/flip.ts';
import {Data} from '../../../zql-integration-tests/src/chinook/fuzz/literals.ts';
import {miniData} from '../../../zql-integration-tests/src/chinook/fuzz/mini.ts';
import {pushForQuery} from '../../../zql-integration-tests/src/chinook/fuzz/push.ts';
import {
  enumerate,
  label,
  lower,
  lowerOr,
} from '../../../zql-integration-tests/src/chinook/fuzz/skeleton.ts';
import {wrapAst} from '../../../zql-integration-tests/src/chinook/fuzz/wrap.ts';
import {schema} from '../../../zql-integration-tests/src/chinook/schema.ts';
import {asQueryInternals} from '../../../zql/src/query/query-internals.ts';
import type {AnyQuery} from '../../../zql/src/query/query.ts';
import {emitGeneratedVectors} from '../vectors.ts';
import {runQuery, schemaJSON, type Push} from './harness.ts';

/**
 * The decorated push lane of the chinook fuzzer restored from rocicorp/mono
 * 7ebeb2172 (`decoratedPushLaneCases` in
 * packages/zql-integration-tests/src/chinook/fuzz/fuzz.test.ts, built the
 * same way from the fuzzer's own modules): every depth-2 skeleton with up to
 * two EXISTS and one related, as an AND and, when it has a gate, an OR, ordered
 * and limited, with each of its flip plans, under the four-phase push history
 * of each table it reads, over the fuzzer's `mini` chinook data. The fuzzer
 * checks the JS view against a fresh hydration after each push; this records
 * the view and its changes, for Swift to match. Skeletons with a NOT EXISTS,
 * which the client's builder rejects, are left out.
 *
 * The fuzzer pushes one row per phase and then two. Pull requests run the
 * one-row lane (about 2,500 cases, 35 MB); `RESTORED_CHINOOK_PUSH_ROWS=1,2`
 * adds the two-row lane, as the nightly does. Both lanes take some 15 s, past
 * vitest's default timeout.
 */
const pushRows = (process.env.RESTORED_CHINOOK_PUSH_ROWS ?? '1')
  .split(',')
  .map(Number);

test('restored chinook decorated push lane', {timeout: 300_000}, () => {
  const data = new Data(miniData, pkOf);
  const skels = enumerate({depth: 2, related: 1, exists: 2});
  const cases = [];
  for (const n of pushRows) {
    for (const s of skels) {
      const shapes: [string, AnyQuery][] = [['and', lower(s)]];
      if (s.children.some(c => c.kind !== 'related')) {
        shapes.push(['or', lowerOr(s)]);
      }
      for (const [shape, lowered] of shapes) {
        const base = applyLimit(applyOrder(lowered, s.table, 'asc1'), 'small');
        const mutations = pushForQuery(data, s, asQueryInternals(base).ast, n);
        if (mutations.length === 0) {
          continue;
        }
        const pushes: Push[] = mutations.map(m =>
          m.kind === 'edit'
            ? {table: m.table, type: 'edit', row: m.row, oldRow: m.old}
            : {table: m.table, type: m.kind, row: m.row},
        );
        const plans: [string, AnyQuery][] = [
          ['', base],
          ...flipVariants(asQueryInternals(base).ast, 2).map(
            ([suffix, flipped]): [string, AnyQuery] => [
              suffix,
              wrapAst(flipped),
            ],
          ),
        ];
        for (const [suffix, query] of plans) {
          const {ast, format} = asQueryInternals(query);
          const result = runQuery(schema, miniData, ast, format, pushes);
          if ('error' in result && result.error.startsWith('not(exists())')) {
            continue;
          }
          cases.push({
            name: `n=${n} decpush|${shape}|${label(s)}${suffix}`,
            ast,
            format,
            pushes,
            result,
          });
        }
      }
    }
  }
  emitGeneratedVectors(
    'restored-chinook-decpush',
    'ivm-ast',
    'The decorated push lane of the chinook fuzzer restored from rocicorp/mono 7ebeb2172, over its mini chinook data. See src/ivm/restored-chinook.test.ts.',
    cases,
    {schema: schemaJSON(schema), data: miniData},
  );
});
