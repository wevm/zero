/**
 * Aggregate bug hunt: targeted IVM cases over hand-made datasets with the
 * values a port most likely gets wrong (big and odd numbers, strings that
 * order differently by code point and code unit, odd JSON paths, null and
 * mixed group keys), and their pushes. `BugHuntTests.swift` replays each.
 */
import {test} from 'vitest';
import type {JSONValue} from '../../../shared/src/json.ts';
import type {Row} from '../../../zero-protocol/src/data.ts';
import {schema} from '../../../zql/src/query/test/test-schemas.ts';
import {emitVectors} from '../vectors.ts';
import {
  buildProgram,
  runQuery,
  schemaJSON,
  type Program,
  type Push,
} from './harness.ts';

type Op = Program['ops'][number];
const p = (table: string, ...ops: Op[]): Program => ({table, ops});
const count: Op = {op: 'count'};
const agg = (
  fn: 'sum' | 'avg' | 'min' | 'max',
  field: string,
  path?: (string | number)[],
): Op => ({op: fn, field, ...(path ? {path} : {})});
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
const distinct = (...columns: string[]): Op => ({op: 'distinct', columns});
const related = (rel: string, ...sub: Op[]): Op => ({op: 'related', rel, sub});
const whereJson = (
  path: (string | number)[],
  op: string,
  value: JSONValue,
): Op =>
  ({op: 'whereExpr', expr: {cmpJson: ['metadata', path, op, value]}}) as Op;

const issue = (
  id: string,
  f: {
    title?: string;
    closed?: boolean;
    ownerId?: string | null;
    createdAt?: number;
  } = {},
): Row => ({
  id,
  title: f.title ?? id,
  description: '',
  closed: f.closed ?? false,
  ownerId: f.ownerId ?? null,
  createdAt: f.createdAt ?? 1,
});
const comment = (
  id: string,
  issueId: string,
  f: {text?: string; authorId?: string; createdAt?: number} = {},
): Row => ({
  id,
  authorId: f.authorId ?? 'u1',
  issueId,
  text: f.text ?? '',
  createdAt: f.createdAt ?? 1,
});
const user = (id: string, metadata: JSONValue, name = id): Row => ({
  id,
  name,
  metadata,
});

type Dataset = {data: Record<string, Row[]>; pushes: Push[]};

const add = (table: string, row: Row): Push => ({table, type: 'add', row});
const remove = (table: string, row: Row): Push => ({
  table,
  type: 'remove',
  row,
});
const edit = (table: string, oldRow: Row, row: Row): Push => ({
  table,
  type: 'edit',
  row,
  oldRow,
});

// Strings whose order differs between code points (UTF-8) and code units
// (UTF-16), or that Swift's String treats as equal.
const strings = [
  '',
  'a',
  'A',
  '\u00e9',
  'e\u0301',
  '\ue000',
  '\u{1f600}',
  '\uffff',
  '\u{10ffff}',
  'Z',
  'z',
];

const bigNumbers = [2 ** 53, 1, 1, 2 ** 53 + 2, -(2 ** 53), 0.1, 0.2, 1e308];

const datasets: Dataset[] = [];
const caseList: [string, number, Program][] = [];
function dataset(d: Dataset, ...programs: [string, Program][]) {
  const i = datasets.push(d) - 1;
  for (const [name, program] of programs) caseList.push([name, i, program]);
}

// Strings: min/max and groups of strings Swift may equate or order otherwise.
{
  const issues = strings.map((t, i) => issue(`i${i}`, {title: t}));
  dataset(
    {
      data: {issue: issues},
      pushes: [
        remove('issue', issues[8]),
        remove('issue', issues[6]),
        remove('issue', issues[7]),
        add('issue', issue('j1', {title: '\u{1f600}'})),
        edit('issue', issues[3], {...issues[3], title: 'e\u0301'}),
        remove('issue', issues[0]),
        remove('issue', issues[4]),
        edit('issue', issues[1], {...issues[1], title: '\u00e9'}),
      ],
    },
    ['strings min', p('issue', agg('min', 'title'))],
    ['strings max', p('issue', agg('max', 'title'))],
    ['strings distinct', p('issue', distinct('title'))],
    ['strings groupBy count', p('issue', groupBy(['title'], 'count'))],
    ['strings distinct count', p('issue', distinct('title'), count)],
    [
      'strings groupBy max desc',
      p(
        'issue',
        {op: 'orderBy', field: 'title', dir: 'desc'},
        groupBy(['title'], 'max', 'title'),
      ),
    ],
  );
}

// Big and odd numbers: sums past 2^53, cancelling, overflow, -0.
{
  const issues = bigNumbers.map((n, i) =>
    issue(`i${i}`, {createdAt: n, closed: i % 2 === 0}),
  );
  dataset(
    {
      data: {issue: issues},
      pushes: [
        add('issue', issue('j1', {createdAt: 2 ** 60})),
        remove('issue', issues[7]),
        remove('issue', issue('j1', {createdAt: 2 ** 60})),
        edit('issue', issues[0], {...issues[0], createdAt: -0}),
        add('issue', issue('j2', {createdAt: -0, closed: true})),
        add('issue', issue('j3', {createdAt: 0, closed: true})),
        remove('issue', issues[5]),
        remove('issue', issues[6]),
        edit('issue', issues[1], {...issues[1], createdAt: 2 ** 53 + 1}),
      ],
    },
    ['numbers sum', p('issue', agg('sum', 'createdAt'))],
    ['numbers avg', p('issue', agg('avg', 'createdAt'))],
    ['numbers min', p('issue', agg('min', 'createdAt'))],
    ['numbers max', p('issue', agg('max', 'createdAt'))],
    [
      'numbers groupBy sum',
      p('issue', groupBy(['closed'], 'sum', 'createdAt')),
    ],
    [
      'numbers groupBy avg',
      p('issue', groupBy(['closed'], 'avg', 'createdAt')),
    ],
    ['numbers distinct', p('issue', distinct('createdAt'))],
    ['numbers groupBy count', p('issue', groupBy(['createdAt'], 'count'))],
  );
}

// -0 and 0 as group keys.
{
  const issues = [
    issue('i1', {createdAt: -0}),
    issue('i2', {createdAt: 0}),
    issue('i3', {createdAt: 1}),
  ];
  dataset(
    {
      data: {issue: issues},
      pushes: [
        add('issue', issue('j1', {createdAt: 0})),
        add('issue', issue('j2', {createdAt: -0})),
        remove('issue', issues[0]),
        remove('issue', issues[1]),
        remove('issue', issue('j1', {createdAt: 0})),
        remove('issue', issue('j2', {createdAt: -0})),
      ],
    },
    ['zero groupBy count', p('issue', groupBy(['createdAt'], 'count'))],
    ['zero distinct', p('issue', distinct('createdAt'))],
    ['zero distinct count', p('issue', distinct('createdAt'), count)],
    ['zero min', p('issue', agg('min', 'createdAt'))],
  );
}

// JSON paths: odd keys, arrays vs objects, deep nesting, mixed leaves.
{
  let deep: JSONValue = 5;
  for (let i = 0; i < 40; i++) deep = {k: deep};
  const users = [
    user('u1', {'': 1, '0': 2, 'a.b': 3, '"q"': 4, '\u00e9': 5, 'p': 'x'}),
    user('u2', {
      '': 'b',
      '0': 'a',
      'a.b': -1,
      '"q"': 'z',
      'e\u0301': 6,
      'p': 2,
    }),
    user('u3', [10, '20', [30]]),
    user('u4', {'0': [7], 'p': true, 'deep': deep}),
    user('u5', {'p': null, '': null, 'p2': {'': {'': 9}}}),
    user('u6', 'scalar'),
    user('u7', null),
    user('u8', {'p': '\u{1f600}', '': '\ue000', 'p2': {'': {'': -9}}}),
    user('u9', {'p': 1e21, '0': -0, '': 2 ** 53}),
  ];
  const paths: [string, (string | number)[]][] = [
    ['empty key', ['']],
    ['key 0', ['0']],
    ['index 0', [0]],
    ['dotted key', ['a.b']],
    ['quoted key', ['"q"']],
    ['precomposed key', ['\u00e9']],
    ['decomposed key', ['e\u0301']],
    ['mixed leaf', ['p']],
    ['nested empty keys', ['p2', '', '']],
    ['index into nested array', [2, 0]],
    ['key 0 then index', ['0', 0]],
    ['deep', ['deep', ...Array(40).fill('k')]],
  ];
  const programs: [string, Program][] = [];
  for (const [n, path] of paths) {
    for (const fn of ['sum', 'avg', 'min', 'max'] as const) {
      programs.push([`json ${n} ${fn}`, p('user', agg(fn, 'metadata', path))]);
    }
    programs.push([
      `json ${n} groupBy max`,
      p('user', groupBy(['name'], 'max', 'metadata', path)),
    ]);
    for (const [op, value] of [
      ['=', 1],
      ['>', 0],
      ['<', 'z'],
      ['IS', null],
      ['IS NOT', null],
      ['!=', 'a'],
    ] as const) {
      programs.push([
        `json ${n} where ${op} ${JSON.stringify(value)}`,
        p('user', whereJson(path, op, value), count),
      ]);
    }
  }
  dataset(
    {
      data: {user: users},
      pushes: [
        edit('user', users[0], {...users[0], metadata: {'': 7, 'p': 'y'}}),
        remove('user', users[7]),
        add('user', user('w1', {'': 'zz', 'p': 'a', '0': 1}, 'u1')),
        edit('user', users[1], {...users[1], metadata: [1]}),
        remove('user', users[8]),
        edit('user', users[3], {...users[3], metadata: {p: 3}}),
        remove('user', user('w1', {'': 'zz', 'p': 'a', '0': 1}, 'u1')),
        remove('user', {...users[0], metadata: {'': 7, 'p': 'y'}}),
      ],
    },
    ...programs,
  );
}

// Groups of several columns with nulls; groups that empty and refill; ties.
{
  const issues = [
    issue('i1', {ownerId: null, closed: false, createdAt: 3}),
    issue('i2', {ownerId: null, closed: true, createdAt: 3}),
    issue('i3', {ownerId: 'u1', closed: false, createdAt: 5}),
    issue('i4', {ownerId: 'u1', closed: false, createdAt: 5}),
    issue('i5', {ownerId: 'null', closed: false, createdAt: 2}),
    issue('i6', {ownerId: '', closed: true, createdAt: 9}),
  ];
  const pushes: Push[] = [
    remove('issue', issues[0]),
    add('issue', issue('j1', {ownerId: null, closed: false, createdAt: 1})),
    edit('issue', issues[2], {...issues[2], ownerId: null}),
    edit('issue', issues[1], {...issues[1], closed: false}),
    remove('issue', issues[3]),
    edit('issue', issues[4], {...issues[4], ownerId: 'u1'}),
    remove('issue', issue('j1', {ownerId: null, closed: false, createdAt: 1})),
    edit('issue', {...issues[2], ownerId: null}, {...issues[2], createdAt: 2}),
    remove('issue', {...issues[2], createdAt: 2}),
    remove('issue', {...issues[1], closed: false}),
    add('issue', issue('j2', {ownerId: null, closed: true, createdAt: 4})),
  ];
  const programs: [string, Program][] = [];
  for (const fn of ['count', 'sum', 'avg', 'min', 'max'] as const) {
    const f = fn === 'count' ? undefined : 'createdAt';
    programs.push([
      `groups ownerId ${fn}`,
      p('issue', groupBy(['ownerId'], fn, f)),
    ]);
    programs.push([
      `groups ownerId closed ${fn}`,
      p('issue', groupBy(['ownerId', 'closed'], fn, f)),
    ]);
    programs.push([
      `groups closed ownerId desc ${fn}`,
      p(
        'issue',
        {op: 'orderBy', field: 'ownerId', dir: 'desc'},
        groupBy(['closed', 'ownerId'], fn, f),
      ),
    ]);
    programs.push([
      `groups where ${fn}`,
      p(
        'issue',
        {op: 'where', field: 'createdAt', cmp: '>', value: 2},
        groupBy(['ownerId'], fn, f),
      ),
    ]);
  }
  programs.push([
    'groups distinct two',
    p('issue', distinct('ownerId', 'closed')),
  ]);
  programs.push([
    'groups distinct two count',
    p('issue', distinct('ownerId', 'closed'), count),
  ]);
  programs.push([
    'groups distinct createdAt',
    p('issue', distinct('createdAt')),
  ]);
  dataset({data: {issue: issues}, pushes}, ...programs);
}

// Relationship aggregates over null correlations and pushes that empty a
// parent's children and refill them.
{
  const issues = [
    issue('i1', {ownerId: 'u1'}),
    issue('i2', {ownerId: null}),
    issue('i3', {ownerId: 'u2'}),
  ];
  const users = [user('u1', {points: 1}), user('u2', {points: 'x'})];
  const comments = [
    comment('c1', 'i1', {createdAt: 2 ** 53, text: '\u{1f600}'}),
    comment('c2', 'i1', {createdAt: 1, text: '\ue000'}),
    comment('c3', 'i3', {createdAt: -0, text: ''}),
  ];
  const programs: [string, Program][] = [];
  for (const fn of ['sum', 'avg', 'min', 'max'] as const) {
    programs.push([
      `related comments ${fn} createdAt`,
      p('issue', related('comments', agg(fn, 'createdAt'))),
    ]);
  }
  programs.push([
    'related comments max text',
    p('issue', related('comments', agg('max', 'text'))),
  ]);
  programs.push([
    'related comments min text',
    p('issue', related('comments', agg('min', 'text'))),
  ]);
  programs.push([
    'related comments count',
    p('issue', related('comments', count)),
  ]);
  programs.push(['related owner count', p('issue', related('owner', count))]);
  programs.push([
    'related owner max json',
    p('issue', related('owner', agg('max', 'metadata', ['points']))),
  ]);
  programs.push([
    'related issues count of users',
    p('user', related('issues', count), related('issues')),
  ]);
  dataset(
    {
      data: {issue: issues, user: users, comment: comments},
      pushes: [
        remove('comment', comments[0]),
        remove('comment', comments[1]),
        add('comment', comment('c4', 'i1', {createdAt: 3, text: 'a'})),
        edit('comment', comments[2], {...comments[2], issueId: 'i1'}),
        edit('issue', issues[1], {...issues[1], ownerId: 'u1'}),
        edit('user', users[1], {...users[1], metadata: {points: 4}}),
        remove('user', users[0]),
        add('user', user('u1', {points: '1'})),
        add('comment', comment('c5', 'i2', {createdAt: 1e308})),
        add('comment', comment('c6', 'i2', {createdAt: -1e308})),
      ],
    },
    ...programs,
  );
}

// Aggregates of columns JS can't reduce: a json column without a path, a
// string or boolean summed, booleans min'd.
{
  const users = [
    user('u1', {points: 1}),
    user('u2', [1]),
    user('u3', null),
    user('u4', 'x'),
    user('u5', 3),
  ];
  const issues = [
    issue('i1', {ownerId: 'u1', closed: true}),
    issue('i2', {ownerId: 'u2'}),
    issue('i3', {ownerId: 'u4', closed: true}),
  ];
  dataset(
    {
      data: {user: users, issue: issues},
      pushes: [
        remove('user', users[1]),
        add('user', user('w1', 2)),
        edit('issue', issues[0], {...issues[0], closed: false}),
      ],
    },
    ['bad max json column', p('user', agg('max', 'metadata'))],
    ['bad min json column', p('user', agg('min', 'metadata'))],
    ['bad sum json column', p('user', agg('sum', 'metadata'))],
    ['bad avg json column', p('user', agg('avg', 'metadata'))],
    [
      'bad groupBy max json column',
      p('user', groupBy(['name'], 'max', 'metadata')),
    ],
    [
      'bad related max json column',
      p('issue', related('owner', agg('max', 'metadata'))),
    ],
    ['bad sum string', p('issue', agg('sum', 'title'))],
    ['bad avg string', p('issue', agg('avg', 'title'))],
    ['bad sum boolean', p('issue', agg('sum', 'closed'))],
    [
      'bad related sum string',
      p('issue', related('owner', agg('sum', 'name'))),
    ],
    ['bad groupBy sum string', p('issue', groupBy(['closed'], 'sum', 'title'))],
    ['min boolean', p('issue', agg('min', 'closed'))],
    ['max boolean', p('issue', agg('max', 'closed'))],
    ['groupBy max boolean', p('issue', groupBy(['ownerId'], 'max', 'closed'))],
    ['groupBy boolean count', p('issue', groupBy(['closed'], 'count'))],
    ['sum unknown column', p('issue', agg('sum', 'nope'))],
    ['max unknown column', p('issue', agg('max', 'nope'))],
    ['groupBy unknown column', p('issue', groupBy(['nope'], 'count'))],
    [
      'groupBy sum unknown field',
      p('issue', groupBy(['closed'], 'sum', 'nope')),
    ],
    [
      'related max unknown column',
      p('issue', related('comments', agg('max', 'nope'))),
    ],
    ['json path on string column', p('issue', agg('max', 'title', ['a']))],
    [
      'groupBy sum of a group column',
      p('issue', groupBy(['createdAt'], 'sum', 'createdAt')),
    ],
    [
      'groupBy max of a group column',
      p('issue', groupBy(['title', 'closed'], 'max', 'title')),
    ],
  );
}

type Case = {
  name: string;
  dataset: number;
  program: Program;
  ast?: unknown;
  format?: unknown;
  result?: unknown;
  builderError?: string;
};

test('aggregate bug hunt ivm', () => {
  const cases: Case[] = caseList.map(([name, dataset, program]) => {
    try {
      const built = buildProgram(schema, program);
      return {
        name,
        dataset,
        program,
        ast: built.ast,
        format: built.format,
        result: runQuery(
          schema,
          datasets[dataset].data,
          built.ast,
          built.format,
          datasets[dataset].pushes,
        ),
      };
    } catch (e) {
      return {name, dataset, program, builderError: (e as Error).message};
    }
  });
  emitVectors(
    'bughunt-ivm',
    'ivm',
    'Aggregate bug hunt: targeted aggregate, distinct and groupBy queries over hand-made datasets. See src/ivm/bughunt.test.ts.',
    cases,
    {schema: schemaJSON(schema), datasets},
    {compact: true},
  );
});
