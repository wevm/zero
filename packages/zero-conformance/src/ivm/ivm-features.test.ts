/**
 * IVM vectors for the query features of sync protocols 54–57 (JSON path
 * filters, aggregates, distinct and groupBy), over the zql test schema with
 * datasets whose `user.metadata` holds varied JSON. A program the JS builder
 * rejects records its error, which the Swift builder must report too.
 */
import {test} from 'vitest';
import type {JSONValue} from '../../../shared/src/json.ts';
import type {Row} from '../../../zero-protocol/src/data.ts';
import type {Schema} from '../../../zero-types/src/schema.ts';
import {schema} from '../../../zql/src/query/test/test-schemas.ts';
import {Random} from '../random.ts';
import {emitGeneratedVectors, emitVectors} from '../vectors.ts';
import {makeData, makeProgram, makePushes} from './generate.ts';
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
  field = 'metadata',
): Program['ops'][number] => ({
  op: 'whereExpr',
  expr: json(path, op, value, field),
});

type Op = Program['ops'][number];
const count: Op = {op: 'count'};
const agg = (
  fn: 'sum' | 'avg' | 'min' | 'max',
  field: string,
  path?: (string | number)[],
): Op => ({op: fn, field, ...(path ? {path} : {})});
const distinct = (...columns: string[]): Op => ({op: 'distinct', columns});
const groupBy = (
  columns: string[],
  fn: 'count' | 'sum' | 'avg' | 'min' | 'max',
  field?: string,
  path?: (string | number)[],
): Op => ({
  op: 'groupBy',
  columns,
  fn,
  ...(field ? {field} : {}),
  ...(path ? {path} : {}),
});
const related = (rel: string, ...sub: Op[]): Op => ({op: 'related', rel, sub});
const orderBy = (field: string, dir: 'asc' | 'desc'): Op => ({
  op: 'orderBy',
  field,
  dir,
});
const limit = (n: number): Op => ({op: 'limit', n});
const start = (row: Row): Op => ({op: 'start', row});

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

  // Top-level aggregates.
  ['count', p('issue', count)],
  [
    'count where',
    p('issue', {op: 'where', field: 'closed', value: true}, count),
  ],
  ['count limited', p('issue', orderBy('createdAt', 'desc'), limit(3), count)],
  ['count started', p('comment', start({id: 'c'}), count)],
  ['count exists', p('issue', {op: 'whereExists', rel: 'comments'}, count)],
  ['count json where', p('user', whereJson(['points'], '>', 0), count)],
  ['sum', p('comment', agg('sum', 'createdAt'))],
  ['avg', p('issue', agg('avg', 'createdAt'))],
  ['min', p('issue', agg('min', 'createdAt'))],
  ['max string', p('issue', agg('max', 'title'))],
  ['min nullable', p('issue', agg('min', 'ownerId'))],
  ['sum json', p('user', agg('sum', 'metadata', ['points']))],
  ['avg json', p('user', agg('avg', 'metadata', ['points']))],
  ['min json', p('user', agg('min', 'metadata', ['points']))],
  ['max json', p('user', agg('max', 'metadata', ['registrar']))],
  ['max json index', p('user', agg('max', 'metadata', ['tags', 0]))],

  // distinct and groupBy.
  ['distinct', p('issue', distinct('closed'))],
  ['distinct nullable', p('issue', distinct('ownerId'))],
  ['distinct two', p('issue', distinct('closed', 'ownerId'))],
  [
    'distinct ordered',
    p('issue', orderBy('ownerId', 'desc'), distinct('closed', 'ownerId')),
  ],
  [
    'distinct where',
    p('comment', whereJson(['x'], '=', 1, 'text'), distinct('authorId')),
  ],
  [
    'distinct exists',
    p('comment', {op: 'whereExists', rel: 'author'}, distinct('issueId')),
  ],
  ['distinct count', p('comment', distinct('authorId'), count)],
  ['distinct two count', p('issue', distinct('closed', 'ownerId'), count)],
  ['groupBy count', p('issue', groupBy(['closed'], 'count'))],
  ['groupBy sum', p('comment', groupBy(['authorId'], 'sum', 'createdAt'))],
  ['groupBy avg', p('issue', groupBy(['ownerId'], 'avg', 'createdAt'))],
  ['groupBy min', p('comment', groupBy(['issueId'], 'min', 'createdAt'))],
  [
    'groupBy max string',
    p('comment', groupBy(['issueId', 'authorId'], 'max', 'text')),
  ],
  [
    'groupBy sum json',
    p('user', groupBy(['name'], 'sum', 'metadata', ['points'])),
  ],
  [
    'groupBy min json',
    p('user', groupBy(['name'], 'min', 'metadata', ['points'])),
  ],
  [
    'groupBy ordered',
    p(
      'issue',
      orderBy('closed', 'desc'),
      groupBy(['ownerId', 'closed'], 'count'),
    ),
  ],

  // Relationship aggregates.
  ['related count', p('issue', related('comments', count))],
  ['related sum', p('issue', related('comments', agg('sum', 'createdAt')))],
  ['related avg', p('user', related('issues', agg('avg', 'createdAt')))],
  ['related min', p('issue', related('comments', agg('min', 'createdAt')))],
  ['related max string', p('issue', related('comments', agg('max', 'text')))],
  [
    'related count where',
    p(
      'issue',
      related('comments', {op: 'where', field: 'authorId', value: 'u1'}, count),
    ),
  ],
  [
    'related count exists',
    p(
      'issue',
      related('comments', {op: 'whereExists', rel: 'revisions'}, count),
    ),
  ],
  ['related count limited', p('issue', related('comments', limit(2), count))],
  ['related one count', p('issue', related('owner', count))],
  [
    'related json sum',
    p('issue', related('owner', agg('sum', 'metadata', ['points']))),
  ],
  [
    'related nested count',
    p('issue', related('comments', related('revisions', count))),
  ],
  [
    'related count and rows',
    p(
      'issue',
      related('comments', count),
      related('owner'),
      {op: 'where', field: 'closed', value: false},
      orderBy('title', 'asc'),
      limit(4),
    ),
  ],
  ['junction count', p('issue', related('labels', count))],
  [
    'junction count where',
    p(
      'issue',
      related(
        'labels',
        {op: 'where', field: 'name', cmp: '>', value: 'b'},
        count,
      ),
    ),
  ],
  ['junction min', p('issue', related('labels', agg('min', 'name')))],
  ['junction max', p('issue', related('labels', agg('max', 'name')))],
  ['junction sum', p('label', related('issues', agg('sum', 'createdAt')))],
  [
    'junction avg where',
    p(
      'label',
      related(
        'issues',
        {op: 'where', field: 'closed', value: true},
        agg('avg', 'createdAt'),
      ),
    ),
  ],

  // The builder rejects these.
  ['count then limit', p('issue', count, limit(1))],
  ['count then one', p('issue', count, {op: 'one'})],
  ['count then start', p('issue', count, start({id: 'a'}))],
  ['count twice', p('issue', count, count)],
  ['sum json empty path', p('user', agg('sum', 'metadata', []))],
  ['sum json negative index', p('user', agg('sum', 'metadata', [-1]))],
  ['distinct then limit', p('issue', distinct('closed'), limit(1))],
  ['distinct then related', p('issue', distinct('closed'), related('owner'))],
  [
    'distinct then sum',
    p('issue', distinct('closed'), agg('sum', 'createdAt')),
  ],
  [
    'distinct then distinct',
    p('issue', distinct('closed'), distinct('ownerId')),
  ],
  ['distinct no columns', p('issue', distinct())],
  ['distinct unknown column', p('issue', distinct('nope'))],
  ['distinct json column', p('user', distinct('metadata'))],
  ['distinct twice named', p('issue', distinct('closed', 'closed'))],
  ['distinct after limit', p('issue', limit(2), distinct('closed'))],
  ['distinct after related', p('issue', related('owner'), distinct('closed'))],
  [
    'distinct after orderBy',
    p('issue', orderBy('title', 'asc'), distinct('closed')),
  ],
  [
    'distinct then orderBy',
    p('issue', distinct('closed'), orderBy('title', 'asc')),
  ],
  ['groupBy count then count', p('issue', groupBy(['closed'], 'count'), count)],
  ['groupBy json column', p('user', groupBy(['metadata'], 'count'))],
  ['groupBy then limit', p('issue', groupBy(['closed'], 'count'), limit(1))],
  ['related distinct', p('issue', related('comments', distinct('authorId')))],
  [
    'related groupBy',
    p('issue', related('comments', groupBy(['authorId'], 'count'))),
  ],
  [
    'exists count',
    p('issue', {op: 'whereExists', rel: 'comments', sub: [count]}),
  ],
  [
    'exists distinct',
    p('issue', {
      op: 'whereExists',
      rel: 'comments',
      sub: [distinct('authorId')],
    }),
  ],
  [
    'exists junction count',
    p('issue', {op: 'whereExists', rel: 'labels', sub: [count]}),
  ],
  [
    'related aggregate nested',
    p('comment', related('issue', related('owner'), count)),
  ],
  [
    'junction aggregate nested',
    p('issue', related('labels', related('issues'), count)),
  ],
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

/** Columns of `table` an aggregate can take: `[column, is a number]`. */
const aggregatable: Record<string, [string, boolean][]> = {
  issue: [
    ['createdAt', true],
    ['title', false],
    ['ownerId', false],
  ],
  comment: [
    ['createdAt', true],
    ['text', false],
  ],
  user: [['name', false]],
  label: [['name', false]],
  issueLabel: [['labelId', false]],
  revision: [['text', false]],
};

/** Group columns of `table`. */
const groupable: Record<string, string[]> = {
  issue: ['closed', 'ownerId', 'title'],
  comment: ['authorId', 'issueId'],
  user: ['name'],
  label: ['name'],
  issueLabel: ['issueId', 'labelId'],
  revision: ['authorId', 'commentId'],
};

/** A random aggregate of `table`'s rows. */
function randomAggregate(r: Random, table: string): Op {
  const fn = r.pick(['count', 'sum', 'avg', 'min', 'max'] as const);
  if (fn === 'count') {
    return count;
  }
  if (table === 'user' && r.bool(0.7)) {
    return agg(
      fn,
      'metadata',
      r.pick([['points'], ['registrar'], ['tags', 0]]),
    );
  }
  const columns = aggregatable[table].filter(
    ([, numeric]) => numeric || fn === 'min' || fn === 'max',
  );
  return columns.length === 0 ? count : agg(fn, r.pick(columns)[0]);
}

const relationships: Schema['relationships'] = schema.relationships;

const destOf = (table: string, rel: string): string =>
  relationships[table]?.[rel]?.at(-1)?.destSchema ?? table;

/**
 * A random program of `makeProgram` with an aggregate or grouping added:
 * on the query itself, or on a relationship. Programs the builder rejects
 * are left out.
 */
function aggregateProgram(r: Random, data: Record<string, Row[]>): Program {
  const base = makeProgram(r, schema, data);
  const {table} = base;
  const k = r.int(4);
  if (k === 0) {
    return {table, ops: [...base.ops, randomAggregate(r, table)]};
  }
  if (k === 1) {
    const columns = groupable[table];
    const picked = columns.filter(() => r.bool(0.5));
    const cols = picked.length > 0 ? picked : [r.pick(columns)];
    // A grouping takes no bound, relationship or other order.
    const ops = base.ops.filter(o => o.op.startsWith('where'));
    if (r.bool(0.3)) {
      ops.push({
        op: 'orderBy',
        field: r.pick(cols),
        dir: r.pick(['asc', 'desc']),
      });
    }
    if (r.bool()) {
      ops.push(distinct(...cols));
      if (r.bool(0.3)) {
        ops.push(count);
      }
      return {table, ops};
    }
    const a = randomAggregate(r, table);
    ops.push(
      a.op === 'count'
        ? groupBy(cols, 'count')
        : groupBy(
            cols,
            a.op as 'sum',
            (a as {field: string}).field,
            (a as {path?: (string | number)[]}).path,
          ),
    );
    return {table, ops};
  }
  const rels = Object.keys(relationships[table] ?? {});
  const rel = r.pick(rels);
  const dest = destOf(table, rel);
  const sub: Op[] = [];
  if (r.bool(0.4)) {
    sub.push({
      op: 'where',
      field: groupable[dest][0],
      cmp: r.pick(['=', '!=', '>', '<'] as const),
      value: 'b',
    });
  }
  if (r.bool(0.2) && k === 2) {
    sub.push(limit(r.pick([1, 2])));
  }
  sub.push(randomAggregate(r, dest));
  return {table, ops: [...base.ops, related(rel, ...sub)]};
}

// IVM_FUZZ_SEED and IVM_FUZZ_CASES widen the search for a local deep run.
const fuzzSeed = Number(process.env.IVM_FUZZ_SEED ?? 57);
const fuzzCases = Number(process.env.IVM_FUZZ_CASES ?? 600);

test('ivm features fuzz', () => {
  const sets = datasets(5757, 8, 25);
  const r = new Random(fuzzSeed);
  const cases: Case[] = [];
  let attempts = 0;
  while (cases.length < fuzzCases && attempts < fuzzCases * 8) {
    attempts++;
    const dataset = r.int(sets.length);
    const program = aggregateProgram(r, sets[dataset].data);
    const c = makeCase(`fuzz ${attempts}`, dataset, sets[dataset], program);
    if (c.result && !('error' in c.result)) {
      cases.push(c);
    }
  }
  emitGeneratedVectors(
    'ivm-features-fuzz',
    'ivm',
    'Seeded random queries with an aggregate, distinct or groupBy, and pushes, over the zql test schema.',
    cases,
    {schema: schemaJSON(schema), datasets: sets},
  );
});
