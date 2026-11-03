/**
 * Aggregate bug hunt: seeded random client scenarios over the synced
 * aggregate queries of `app.ts`. A scenario syncs a few queries, then mixes
 * local mutations (with `tx.run` reads of the aggregates), server
 * confirmations of some of them, other clients' writes, dropped and
 * re-added queries and restarts. Every server row comes from JS's own
 * pipeline (`server-rows.ts`), as zero-cache would stream it.
 *
 * `AGG_FUZZ_SEED` and `AGG_FUZZ_CASES` widen the search. The corpus goes to
 * `generated/client-aggregates-fuzz.json`; `BugHuntTests.swift` replays it.
 */
import {test} from 'vitest';
import type {ReadonlyJSONValue} from '../../../shared/src/json.ts';
import {interestingStrings, Random} from '../random.ts';
import {emitGeneratedVectors, emitVectors} from '../vectors.ts';
import {
  runScenario,
  START_TIME,
  type Scenario,
  type Step,
  type StepResult,
} from './harness.ts';
import {bughuntScenarios} from './scenarios/bughunt.ts';
import {advance, connected, queryHash, start} from './scenarios/helpers.ts';
import {syncedRows, type ServerData, type SyncedRow} from './server-rows.ts';

type JSONObject = {readonly [key: string]: ReadonlyJSONValue};
type QueryRef = {name: string; args?: ReadonlyJSONValue};
type CRUD = {
  op: 'insert' | 'upsert' | 'update' | 'delete';
  table: string;
  value: JSONObject;
};

/** The aggregate queries a scenario syncs some of. */
export const aggregateRefs: QueryRef[] = [
  ...[
    'aggregates.issueCount',
    'aggregates.openIssueCount',
    'aggregates.commentCounts',
    'aggregates.commentTimes',
    'aggregates.labelCounts',
    'aggregates.owners',
    'aggregates.commentsByIssue',
    'aggregates.timeByAuthor',
    'aggregates.openByState',
    'aggregates.issueStates',
    'aggregates.issueStateCount',
    'aggregates.countByState',
    'aggregates.timeByState',
    'aggregates.avgTimeByState',
    'aggregates.issuesByOwner',
    'aggregates.latestIssueTime',
    'aggregates.commentAndLabelCounts',
    'aggregates.commentTimeSums',
    'aggregates.userIssueCounts',
    'aggregates.pointsByName',
    'aggregates.avgPointsByName',
    'aggregates.labelMax',
    'aggregates.bugLabelCounts',
    'bughunt.maxCommentText',
    'bughunt.minIssueTitle',
    'bughunt.pointsSum',
    'bughunt.ownerIssueCounts',
    'bughunt.commentAvgWhere',
    'bughunt.commentsByAuthorAndIssue',
    'bughunt.avgTimeByIssueWhere',
    'bughunt.commentCountsLimited',
    'bughunt.ownerCount',
    'bughunt.sumTimeByOwner',
  ].map((name): QueryRef => ({name})),
  {name: 'aggregates.issueWithCommentCount', args: 'i1'},
  {name: 'aggregates.issueWithCommentCount', args: 'i2'},
];

/** Queries of rows, which give the client rows to edit and delete. */
const rowRefs: QueryRef[] = [
  {name: 'issues.all'},
  {name: 'users.all'},
  {name: 'bughunt.allComments'},
  {name: 'issueLabels.all'},
  {name: 'bughunt.allLabels'},
];

const issueIDs = ['i1', 'i2', 'i3', 'i4', 'i5'];
const commentIDs = ['c1', 'c2', 'c3', 'c4', 'c5', 'c6', 'c7', 'c8'];
const userIDs = ['u1', 'u2', 'u3'];
const labelIDs = ['l1', 'l2', 'l3'];
const numbers = [0, 1, 2, 3, 4, 5, 7, 0.1, 0.2, 0.3, -1, 2.5, 2 ** 53, 1e21];
const names = ['A', 'B', 'a', '', '\u00e9', 'e\u0301', '\u{1f600}'];
const texts = ['', 'a', 'b', 'B', 'z', ...interestingStrings.slice(14, 30)];

function metadata(r: Random): ReadonlyJSONValue {
  return r.pick<ReadonlyJSONValue>([
    null,
    {points: r.pick(numbers)},
    {points: r.pick(numbers)},
    {points: '7'},
    {other: 1},
    {points: null},
    [1],
  ]);
}

/** A full row of `table` in client names. */
function clientRow(r: Random, table: string, id: string): JSONObject {
  switch (table) {
    case 'issue':
      return {
        id,
        title: r.pick(texts),
        description: '',
        closed: r.bool(),
        ownerId: r.bool(0.2) ? null : r.pick([...userIDs, 'u9']),
        createdAt: r.pick(numbers),
      };
    case 'comment':
      return {
        id,
        authorId: r.pick(userIDs),
        issueId: r.bool(0.1) ? 'i9' : r.pick(issueIDs),
        text: r.pick(texts),
        createdAt: r.pick(numbers),
      };
    case 'user':
      return {id, name: r.pick(names), metadata: metadata(r)};
    case 'label':
      return {id, name: r.pick(['bug', 'ui', 'a', 'z'])};
    default:
      throw new Error(table);
  }
}

const serverTable: Record<string, string> = {
  issue: 'issues',
  comment: 'comments',
  user: 'users',
  label: 'label',
  issueLabel: 'issueLabel',
};
const serverColumn: Record<string, string> = {
  'issue.ownerId': 'owner_id',
  'comment.issueId': 'issue_id',
};
const primaryKey: Record<string, string[]> = {
  issue: ['id'],
  comment: ['id'],
  user: ['id'],
  label: ['id'],
  issueLabel: ['issueId', 'labelId'],
};

function toServer(table: string, row: JSONObject): JSONObject {
  return Object.fromEntries(
    Object.entries(row).map(([k, v]) => [
      serverColumn[`${table}.${k}`] ?? k,
      v,
    ]),
  );
}

/** Applies a CRUD op, as the server's mutator would, to `data`. */
function applyServer(data: ServerData, o: CRUD): ServerData {
  const t = serverTable[o.table];
  const rows = data[t] ?? [];
  const pk = primaryKey[o.table].map(k => serverColumn[`${o.table}.${k}`] ?? k);
  const value = toServer(o.table, o.value);
  const i = rows.findIndex(row => pk.every(k => row[k] === value[k]));
  let next = rows;
  switch (o.op) {
    case 'insert':
      if (i < 0) next = [...rows, value];
      break;
    case 'upsert':
      next =
        i < 0
          ? [...rows, value]
          : rows.map((row, j) => (j === i ? {...row, ...value} : row));
      break;
    case 'update':
      if (i >= 0) {
        next = rows.map((row, j) => (j === i ? {...row, ...value} : row));
      }
      break;
    case 'delete':
      if (i >= 0) next = rows.filter((_, j) => j !== i);
      break;
  }
  return {...data, [t]: next};
}

function randomData(r: Random): ServerData {
  const data: ServerData = {};
  const some = (ids: string[]) => ids.filter(() => r.bool(0.7));
  data['issues'] = some(issueIDs).map(id =>
    toServer('issue', clientRow(r, 'issue', id)),
  );
  data['comments'] = some(commentIDs).map(id =>
    toServer('comment', clientRow(r, 'comment', id)),
  );
  data['users'] = some(userIDs).map(id => clientRow(r, 'user', id));
  data['label'] = some(labelIDs).map(id => clientRow(r, 'label', id));
  const edges = new Map<string, JSONObject>();
  for (let i = 0; i < 5; i++) {
    const e = {issueId: r.pick(issueIDs), labelId: r.pick(labelIDs)};
    edges.set(JSON.stringify(e), e);
  }
  data['issueLabel'] = [...edges.values()];
  return data;
}

/** A random CRUD op in client names. */
function randomOp(r: Random): CRUD {
  const table = r.pick([
    'issue',
    'comment',
    'comment',
    'comment',
    'user',
    'issueLabel',
  ]);
  if (table === 'issueLabel') {
    return {
      op: r.pick(['insert', 'delete'] as const),
      table,
      value: {issueId: r.pick(issueIDs), labelId: r.pick(labelIDs)},
    };
  }
  const ids =
    table === 'issue' ? issueIDs : table === 'comment' ? commentIDs : userIDs;
  const id = r.pick(ids);
  const op = r.pick([
    'insert',
    'upsert',
    'update',
    'update',
    'delete',
  ] as const);
  const full = clientRow(r, table, id);
  if (op !== 'update') {
    return {op, table, value: op === 'delete' ? {id} : full};
  }
  // An update of one or two columns.
  const columns = Object.keys(full).filter(c => c !== 'id');
  const picked = columns.filter(() => r.bool(0.4));
  const cols = picked.length > 0 ? picked : [r.pick(columns)];
  return {
    op,
    table,
    value: Object.fromEntries([['id', id], ...cols.map(c => [c, full[c]])]),
  };
}

function rowsKey(row: SyncedRow): string {
  return JSON.stringify([row.table, row.id]);
}

function union(
  refs: QueryRef[],
  data: ServerData,
  version: string,
): Map<string, SyncedRow> {
  const out = new Map<string, SyncedRow>();
  for (const ref of refs) {
    for (const row of syncedRows(ref.name, ref.args, data, version).values()) {
      out.set(rowsKey(row), row);
    }
  }
  return out;
}

function diff(
  old: Map<string, SyncedRow>,
  next: Map<string, SyncedRow>,
): ReadonlyJSONValue[] {
  const patch: ReadonlyJSONValue[] = [];
  for (const [key, row] of old) {
    if (!next.has(key)) {
      patch.push({op: 'del', tableName: row.table, id: row.id});
    }
  }
  for (const [key, row] of next) {
    const prev = old.get(key);
    const same =
      prev !== undefined &&
      JSON.stringify({...prev.value, _0_version: null}) ===
        JSON.stringify({...row.value, _0_version: null});
    if (!same) {
      patch.push({op: 'put', tableName: row.table, value: row.value});
    }
  }
  return patch;
}

const sameRef = (a: QueryRef, b: QueryRef) =>
  a.name === b.name && JSON.stringify(a.args) === JSON.stringify(b.args);

/** A seeded random scenario. */
export function randomScenario(r: Random, name: string): Scenario {
  let data = randomData(r);
  const pickAgg = aggregateRefs.filter(() => r.bool(0.12));
  const aggs = pickAgg.length > 0 ? pickAgg : [r.pick(aggregateRefs)];
  const rows = rowRefs.filter(() => r.bool(0.4));
  // The views, by name, and the queries the server has answered.
  const views = new Map<string, QueryRef>();
  [...aggs, ...rows].forEach((ref, i) => views.set(`v${i}`, ref));
  let got: QueryRef[] = [...views.values()];
  let cookieN = 1;
  const cookie = (n: number) => String(n).padStart(2, '0');
  let last = union(got, data, cookie(1));

  const steps: Step[] = [
    start,
    ...Array.from(views, ([view, ref]): Step => ({
      op: 'materialize',
      view,
      query: ref.name,
      ...(ref.args === undefined ? {} : {args: ref.args}),
    })),
    connected,
    advance(10),
    {
      op: 'receiveAll',
      messages: [
        ['pokeStart', {pokeID: 'p1', baseCookie: null}],
        [
          'pokePart',
          {
            pokeID: 'p1',
            gotQueriesPatch: got.map(ref => ({
              op: 'put',
              hash: queryHash(ref.name, ref.args),
            })),
            rowsPatch: diff(new Map(), last),
          },
        ],
        ['pokeEnd', {pokeID: 'p1', cookie: cookie(1)}],
      ],
    },
    advance(10),
  ];

  // Pending mutations: the client (by restart count) and its mutation ID.
  type Pending = {client: number; id: number; ops: CRUD[]};
  const pending: Pending[] = [];
  let clientIndex = 0;
  let nextID = 1;
  let mutationN = 0;
  let viewN = views.size;

  /** A poke to the next cookie with `part`, the rows moved to `data`. */
  const pokeTo = (part: Record<string, ReadonlyJSONValue>) => {
    const base = cookie(cookieN);
    cookieN++;
    const next = union(got, data, cookie(cookieN));
    const rowsPatch = diff(last, next);
    last = next;
    const pokeID = `p${cookieN}`;
    const messages: ReadonlyJSONValue[] = [
      ['pokeStart', {pokeID, baseCookie: base}],
      ['pokePart', {pokeID, ...part, rowsPatch}],
      ['pokeEnd', {pokeID, cookie: cookie(cookieN)}],
    ];
    // One frame per step, or all in one read.
    if (r.bool()) {
      steps.push({op: 'receiveAll', messages});
    } else {
      for (const message of messages) steps.push({op: 'receive', message});
    }
  };

  const clientKey = (c: number) =>
    c === clientIndex ? '$clientID' : `$clientID.${c}`;

  const actions = 6 + r.int(14);
  for (let a = 0; a < actions; a++) {
    const k = r.next();
    if (k < 0.4) {
      const ops = Array.from({length: 1 + r.int(2)}, () => randomOp(r));
      const reads = [...views.values()]
        .filter(ref => aggregateRefs.some(x => sameRef(x, ref)))
        .filter(() => r.bool(0.4));
      steps.push({
        op: 'mutate',
        mutation: `m${++mutationN}`,
        mutator: 'aggregates.read',
        args: {ops, queries: reads} as unknown as ReadonlyJSONValue,
      });
      pending.push({client: clientIndex, id: nextID++, ops});
      if (r.bool(0.7)) steps.push(advance(10));
    } else if (k < 0.65 && pending.length > 0) {
      // The server confirms the oldest `n` pending mutations.
      const n = 1 + r.int(pending.length);
      const done = pending.splice(0, n);
      for (const m of done) for (const o of m.ops) data = applyServer(data, o);
      if (r.bool(0.3)) data = applyServer(data, randomOp(r));
      const lmids: Record<string, number> = {};
      for (const m of done) lmids[clientKey(m.client)] = m.id;
      pokeTo({lastMutationIDChanges: lmids});
      steps.push(advance(10));
    } else if (k < 0.8) {
      // Another client's write.
      data = applyServer(data, randomOp(r));
      pokeTo({});
      steps.push(advance(10));
    } else if (k < 0.85 && views.size > 1) {
      // A view goes and the server drops its query.
      const [view, ref] = r.pick([...views]);
      views.delete(view);
      steps.push({op: 'destroy', view}, advance(10));
      if (![...views.values()].some(v => sameRef(v, ref))) {
        got = got.filter(g => !sameRef(g, ref));
        pokeTo({
          gotQueriesPatch: [{op: 'del', hash: queryHash(ref.name, ref.args)}],
        });
        steps.push(advance(10));
      }
    } else if (k < 0.9) {
      // A view of an aggregate query (again).
      const ref = r.pick(aggregateRefs);
      const view = `v${viewN++}`;
      views.set(view, ref);
      steps.push({
        op: 'materialize',
        view,
        query: ref.name,
        ...(ref.args === undefined ? {} : {args: ref.args}),
      });
      steps.push(advance(10));
      if (!got.some(g => sameRef(g, ref))) {
        got = [...got, ref];
        pokeTo({
          gotQueriesPatch: [{op: 'put', hash: queryHash(ref.name, ref.args)}],
        });
        steps.push(advance(10));
      }
    } else if (k < 0.95 && process.env.AGG_FUZZ_NO_RESTART !== '1') {
      // A restart: the views come back, and the server catches the client up.
      steps.push(advance(2000), {op: 'restart'});
      clientIndex++;
      nextID = 1;
      for (const [view, ref] of views) {
        steps.push({
          op: 'materialize',
          view,
          query: ref.name,
          ...(ref.args === undefined ? {} : {args: ref.args}),
        });
      }
      steps.push(start, connected, advance(10));
      pokeTo({});
      steps.push(advance(10));
    } else {
      steps.push(advance(10));
    }
  }
  steps.push(advance(10));
  return {name, steps};
}

test('aggregate client bug hunt', {timeout: 600_000}, async () => {
  const cases: (Scenario & {expect: StepResult[]})[] = [];
  for (const scenario of bughuntScenarios) {
    cases.push({...scenario, expect: await runScenario(scenario)});
  }
  emitVectors(
    'bughunt-client',
    'client-scenarios',
    'Aggregate bug hunt: targeted client scenarios for synced aggregates. See src/client/scenarios/bughunt.ts.',
    cases,
    {startTime: START_TIME},
    {compact: true},
  );
});

const seed = Number(process.env.AGG_FUZZ_SEED ?? 1);
const count = Number(process.env.AGG_FUZZ_CASES ?? 150);

test('aggregate client fuzz', {timeout: 3_600_000}, async () => {
  const r = new Random(seed);
  const cases: (Scenario & {expect: StepResult[]})[] = [];
  for (let i = 0; i < count; i++) {
    const scenario = randomScenario(r, `fuzz ${seed}-${i}`);
    cases.push({...scenario, expect: await runScenario(scenario)});
  }
  emitGeneratedVectors(
    'client-aggregates-fuzz',
    'client-scenarios',
    'Seeded random client scenarios over synced aggregates. See src/client/bughunt.test.ts.',
    cases,
    {startTime: START_TIME},
  );
});
