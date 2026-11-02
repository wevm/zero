/**
 * IVM vectors for the query features of sync protocols 54–57 (JSON path
 * filters, aggregates, distinct and groupBy), over the zql test schema with
 * datasets whose `user.metadata` holds varied JSON. A program the JS builder
 * rejects records its error, which the Swift builder must report too.
 */
import {test} from 'vitest';
import type {JSONValue} from '../../../shared/src/json.ts';
import type {Row} from '../../../zero-protocol/src/data.ts';
import {schema} from '../../../zql/src/query/test/test-schemas.ts';
import {Random} from '../random.ts';
import {emitVectors} from '../vectors.ts';
import {makeData, makePushes} from './generate.ts';
import {
  buildProgram,
  runQuery,
  schemaJSON,
  type Expr,
  type Program,
  type Push,
  type QueryResult,
} from './harness.ts';

/** The JSON a `user.metadata` holds. */
const metadataValues: JSONValue[] = [
  null,
  {
    registrar: 'github',
    email: 'a',
    points: 3,
    tags: ['x', 'y'],
    nested: {a: [1, 'b']},
  },
  {registrar: 'google', points: '3', tags: []},
  {points: 2.5, tags: ['y'], nested: {a: [2]}},
  {points: null, nested: {a: 'x'}},
  {registrar: 'github', points: -1, login: 'u'},
  {registrar: true, points: 0},
  [1, 2, 3],
  'github',
  7,
  {'0': 'key', 'points': 1e21, 'registrar': 'Google'},
  {registrar: 'gitlab', points: 3, tags: ['x']},
];

/**
 * The metadata of `row`, a function of its id and generated metadata only,
 * so an edit that leaves the metadata alone leaves it alone here too.
 */
function richMetadata(row: Row): JSONValue {
  const key = String(row['id']) + JSON.stringify(row['metadata'] ?? null);
  let h = 7;
  for (let i = 0; i < key.length; i++) {
    h = (h * 31 + key.charCodeAt(i)) >>> 0;
  }
  return metadataValues[h % metadataValues.length];
}

function enrich(table: string, row: Row): Row {
  return table === 'user' ? {...row, metadata: richMetadata(row)} : row;
}

type Dataset = {data: Record<string, Row[]>; pushes: Push[]};

function datasets(seed: number, count: number, pushes: number): Dataset[] {
  const r = new Random(seed);
  return Array.from({length: count}, () => {
    const data = makeData(r, schema);
    const ps = makePushes(r, schema, data, pushes);
    return {
      data: Object.fromEntries(
        Object.entries(data).map(([t, rows]) => [
          t,
          rows.map(row => enrich(t, row)),
        ]),
      ),
      pushes: ps.map(p => ({
        ...p,
        row: enrich(p.table, p.row),
        ...(p.oldRow ? {oldRow: enrich(p.table, p.oldRow)} : {}),
      })),
    };
  });
}

const p = (table: string, ...ops: Program['ops']): Program => ({table, ops});
const json = (
  path: (string | number)[],
  op: string,
  value: JSONValue,
  field = 'metadata',
): Expr => ({cmpJson: [field, path, op, value]}) as Expr;
const whereJson = (
  path: (string | number)[],
  op: string,
  value: JSONValue,
): Program['ops'][number] => ({
  op: 'whereExpr',
  expr: json(path, op, value),
});

const curated: [string, Program][] = [
  ['json =', p('user', whereJson(['registrar'], '=', 'github'))],
  ['json !=', p('user', whereJson(['registrar'], '!=', 'github'))],
  ['json = bool', p('user', whereJson(['registrar'], '=', true))],
  ['json >', p('user', whereJson(['points'], '>', 2))],
  ['json <=', p('user', whereJson(['points'], '<=', 2.5))],
  ['json < string', p('user', whereJson(['points'], '<', '4'))],
  ['json IN', p('user', whereJson(['points'], 'IN', [3, -1]))],
  ['json NOT IN', p('user', whereJson(['points'], 'NOT IN', [3]))],
  ['json IN empty', p('user', whereJson(['points'], 'IN', []))],
  ['json NOT IN empty', p('user', whereJson(['points'], 'NOT IN', []))],
  ['json IN strings', p('user', whereJson(['registrar'], 'IN', ['google']))],
  ['json IS null', p('user', whereJson(['points'], 'IS', null))],
  ['json IS NOT null', p('user', whereJson(['points'], 'IS NOT', null))],
  ['json IS value', p('user', whereJson(['registrar'], 'IS', 'github'))],
  ['json = null', p('user', whereJson(['points'], '=', null))],
  ['json LIKE', p('user', whereJson(['registrar'], 'LIKE', 'g%'))],
  ['json NOT ILIKE', p('user', whereJson(['registrar'], 'NOT ILIKE', 'G%'))],
  ['json LIKE number', p('user', whereJson(['points'], 'LIKE', '3'))],
  ['json array index', p('user', whereJson(['tags', 0], '=', 'x'))],
  ['json nested path', p('user', whereJson(['nested', 'a', 1], '=', 'b'))],
  ['json object key "0"', p('user', whereJson(['0'], '=', 'key'))],
  ['json index into array', p('user', whereJson([0], '=', 1))],
  ['json key into array', p('user', whereJson(['0'], '=', 1))],
  ['json index into object', p('user', whereJson([0], '=', 'key'))],
  ['json whole object', p('user', whereJson(['nested'], '!=', 'x'))],
  ['json non-json column', p('user', whereJson(['a'], '=', 'x'))],
  [
    'json or column',
    p('user', {
      op: 'whereExpr',
      expr: {
        or: [json(['points'], '>=', 3), {cmp: ['name', '=', 'a']}],
      },
    }),
  ],
  [
    'json in exists',
    p('comment', {
      op: 'whereExists',
      rel: 'author',
      sub: [whereJson(['registrar'], '=', 'github')],
    }),
  ],
  [
    'json in related',
    p('issue', {
      op: 'related',
      rel: 'owner',
      sub: [whereJson(['points'], '>', 0)],
    }),
  ],
  [
    'json ordered and limited',
    p(
      'user',
      whereJson(['registrar'], 'IS NOT', null),
      {op: 'orderBy', field: 'name', dir: 'desc'},
      {op: 'limit', n: 2},
    ),
  ],
  // The builder rejects these.
  ['json negative index', p('user', whereJson([-1], '=', 1))],
  ['json fractional index', p('user', whereJson([1.5], '=', 1))],
  ['json index too large', p('user', whereJson([2 ** 31], '=', 1))],
  ['json empty path', p('user', whereJson([], '=', 1))],
  ['json mixed IN', p('user', whereJson(['points'], 'IN', [3, '3']))],
  ['json mixed NOT IN', p('user', whereJson(['points'], 'NOT IN', [true, 1]))],
];

type Case = {
  name: string;
  dataset: number;
  program: Program;
  ast?: unknown;
  format?: unknown;
  result?: QueryResult;
  builderError?: string;
};

function makeCase(
  name: string,
  dataset: number,
  data: Dataset,
  program: Program,
): Case {
  let built;
  try {
    built = buildProgram(schema, program);
  } catch (e) {
    return {name, dataset, program, builderError: (e as Error).message};
  }
  return {
    name,
    dataset,
    program,
    ast: built.ast,
    format: built.format,
    result: runQuery(schema, data.data, built.ast, built.format, data.pushes),
  };
}

test('ivm features', () => {
  const sets = datasets(57, 4, 30);
  const cases: Case[] = [];
  for (const [name, program] of curated) {
    sets.forEach((set, i) => {
      const c = makeCase(name, i, set, program);
      // A builder error is the same for every dataset.
      if (c.builderError === undefined || i === 0) {
        cases.push(c);
      }
    });
  }
  emitVectors(
    'ivm-features',
    'ivm',
    'Queries with JSON path filters, aggregates, distinct and groupBy over the zql test schema: the view after hydration and after each source push, and the changes the view received; or the error the builder throws.',
    cases,
    {schema: schemaJSON(schema), datasets: sets},
    {compact: true},
  );
});
