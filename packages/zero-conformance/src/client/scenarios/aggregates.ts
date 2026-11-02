/**
 * Synced aggregates, distinct and groupBy: the value before the server sends
 * it, the rows it syncs (computed by JS's pipeline, see `server-rows.ts`),
 * the optimistic moves of a local mutation and the rebase that confirms
 * them, a restart, and the server dropping the query.
 */
import type {ReadonlyJSONValue} from '../../../../shared/src/json.ts';
import type {Scenario, Step} from '../harness.ts';
import {serverRowsPatch, type ServerData} from '../server-rows.ts';
import {
  advance,
  comment,
  connected,
  del,
  gotDel,
  issue,
  mutate,
  poke,
  put,
  queryHash,
  start,
  syncPoke,
  user,
} from './helpers.ts';

const data: ServerData = {
  issues: [
    issue('i1', {ownerId: 'u1', createdAt: 3}),
    issue('i2', {createdAt: 4}),
    issue('i3', {ownerId: 'u1', closed: true, createdAt: 5}),
  ],
  comments: [
    comment('c1', 'i1', {createdAt: 5}),
    comment('c2', 'i1', {authorId: 'u2', createdAt: 7}),
    comment('c3', 'i2', {createdAt: 2}),
    comment('c4', 'i2', {authorId: 'u9', createdAt: 100}),
  ],
  issueLabel: [
    {issueId: 'i1', labelId: 'l1'},
    {issueId: 'i1', labelId: 'l2'},
    {issueId: 'i3', labelId: 'l1'},
  ],
  label: [
    {id: 'l1', name: 'bug'},
    {id: 'l2', name: 'ui'},
  ],
};

/** `data` with `rows` added to `table`. */
function plus(table: string, ...rows: ServerData[string]): ServerData {
  return {...data, [table]: [...(data[table] ?? []), ...rows]};
}

/** `data` with `table`'s row `id` replaced by `row`, or removed. */
function replacing(
  table: string,
  id: string,
  row?: ServerData[string][number],
): ServerData {
  return {
    ...data,
    [table]: (data[table] ?? []).flatMap(r =>
      r['id'] !== id ? [r] : row ? [row] : [],
    ),
  };
}

/** Materializes `query`, connects, and syncs its rows over `data`. */
function synced(query: string, over: ServerData = data, view = 'v'): Step[] {
  return [
    start,
    {op: 'materialize', view, query},
    connected,
    advance(10),
    ...syncPoke(
      'p1',
      null,
      '01',
      [queryHash(query)],
      serverRowsPatch(query, undefined, undefined, over),
    ),
  ];
}

/** The server confirms mutation `lmid` and moves `query`'s rows. */
function confirm(
  query: string,
  before: ServerData,
  after: ServerData,
  lmid: number,
  pokeID = 'p2',
  baseCookie = '01',
  cookie = '02',
) {
  return poke(pokeID, baseCookie, cookie, {
    lastMutationIDChanges: {$clientID: lmid},
    rowsPatch: serverRowsPatch(query, undefined, before, after, cookie),
  });
}

const addComment = (
  mutation: string,
  id: string,
  issueId: string,
  fields: {authorId?: string; createdAt?: number} = {},
) => mutate(mutation, 'comment.add', {id, issueId, ...fields});

const ops = (
  mutation: string,
  o: {op: string; table: string; value: ReadonlyJSONValue}[],
  queries: {name: string; args?: ReadonlyJSONValue}[] = [],
) => mutate(mutation, 'aggregates.read', {ops: o, queries});

export const aggregateScenarios: Scenario[] = [
  {
    name: 'aggregate: a count is 0 until the server sends it, then its synced value',
    steps: [
      start,
      {op: 'materialize', view: 'v', query: 'aggregates.issueCount'},
      connected,
      advance(10),
      ...syncPoke(
        'p1',
        null,
        '01',
        [queryHash('aggregates.issueCount')],
        serverRowsPatch('aggregates.issueCount', undefined, undefined, data),
      ),
      advance(10),
    ],
  },
  {
    name: 'aggregate: a top-level count waits for the server after a local mutation',
    steps: [
      ...synced('aggregates.openIssueCount'),
      mutate('m1', 'issue.close', 'i1'),
      advance(10),
      ...confirm(
        'aggregates.openIssueCount',
        data,
        replacing(
          'issues',
          'i1',
          issue('i1', {ownerId: 'u1', closed: true, createdAt: 3}),
        ),
        1,
      ),
      advance(10),
    ],
  },
  {
    name: 'aggregate: a relationship count moves with a local mutation, and the server confirms it',
    steps: [
      ...synced('aggregates.commentCounts'),
      addComment('m1', 'c5', 'i3'),
      addComment('m2', 'c6', 'i1'),
      advance(10),
      ...confirm(
        'aggregates.commentCounts',
        data,
        plus('comments', comment('c5', 'i3'), comment('c6', 'i1')),
        2,
      ),
      advance(10),
    ],
  },
  {
    name: 'aggregate: the server confirms one mutation while another is still pending',
    steps: [
      ...synced('aggregates.commentCounts'),
      addComment('m1', 'c5', 'i3'),
      addComment('m2', 'c6', 'i3'),
      advance(10),
      ...confirm(
        'aggregates.commentCounts',
        data,
        plus('comments', comment('c5', 'i3')),
        1,
      ),
      advance(10),
    ],
  },
  {
    name: 'aggregate: a comment moved to another issue moves both counts; one deleted moves one',
    steps: [
      ...synced('aggregates.commentCounts'),
      ops('m1', [
        {op: 'update', table: 'comment', value: {id: 'c1', issueId: 'i2'}},
      ]),
      ops('m2', [{op: 'delete', table: 'comment', value: {id: 'c3'}}]),
      advance(10),
    ],
  },
  {
    name: 'aggregate: an average moves only with the comments its where counts',
    steps: [
      ...synced('aggregates.commentTimes'),
      addComment('m1', 'c5', 'i1', {authorId: 'u9', createdAt: 1000}),
      addComment('m2', 'c6', 'i1', {createdAt: 9}),
      addComment('m3', 'c7', 'i3', {createdAt: 4}),
      ops('m4', [
        {op: 'update', table: 'comment', value: {id: 'c4', authorId: 'u1'}},
      ]),
      advance(10),
      ...confirm(
        'aggregates.commentTimes',
        data,
        {
          ...data,
          comments: [
            ...data.comments.map(c =>
              c['id'] === 'c4' ? {...c, authorId: 'u1'} : c,
            ),
            comment('c5', 'i1', {authorId: 'u9', createdAt: 1000}),
            comment('c6', 'i1', {createdAt: 9}),
            comment('c7', 'i3', {createdAt: 4}),
          ],
        },
        4,
      ),
      advance(10),
    ],
  },
  {
    name: "aggregate: a junction count is the server's until it sends another",
    steps: [
      ...synced('aggregates.labelCounts'),
      ops('m1', [
        {
          op: 'insert',
          table: 'issueLabel',
          value: {issueId: 'i2', labelId: 'l1'},
        },
      ]),
      advance(10),
      ...confirm(
        'aggregates.labelCounts',
        data,
        plus('issueLabel', {issueId: 'i2', labelId: 'l1'}),
        1,
      ),
      advance(10),
    ],
  },
  {
    name: 'aggregate: a groupBy count adds a group with its first row and drops it with its last',
    steps: [
      ...synced('aggregates.commentsByIssue'),
      addComment('m1', 'c5', 'i3'),
      ops('m2', [
        {op: 'delete', table: 'comment', value: {id: 'c3'}},
        {op: 'delete', table: 'comment', value: {id: 'c4'}},
      ]),
      addComment('m3', 'c6', 'i1'),
      advance(10),
      ...confirm(
        'aggregates.commentsByIssue',
        data,
        {
          ...data,
          comments: [
            ...data.comments.filter(c => c['id'] !== 'c3' && c['id'] !== 'c4'),
            comment('c5', 'i3'),
            comment('c6', 'i1'),
          ],
        },
        3,
      ),
      advance(10),
    ],
  },
  {
    name: "aggregate: a groupBy sum moves and counts its group's rows",
    steps: [
      ...synced('aggregates.timeByAuthor'),
      addComment('m1', 'c5', 'i1', {authorId: 'u3', createdAt: 6}),
      addComment('m2', 'c6', 'i1', {authorId: 'u2', createdAt: 1}),
      ops('m3', [{op: 'delete', table: 'comment', value: {id: 'c2'}}]),
      advance(10),
    ],
  },
  {
    name: 'aggregate: a distinct adds a value with its first row',
    steps: [
      ...synced('aggregates.owners'),
      mutate('m1', 'issue.create', {id: 'i4', ownerId: 'u7'}),
      mutate('m2', 'issue.create', {id: 'i5', ownerId: 'u1'}),
      mutate('m3', 'issue.update', {id: 'i2', ownerId: 'u1'}),
      advance(10),
      ...confirm(
        'aggregates.owners',
        data,
        {
          ...data,
          issues: [
            issue('i1', {ownerId: 'u1', createdAt: 3}),
            issue('i2', {ownerId: 'u1', createdAt: 4}),
            issue('i3', {ownerId: 'u1', closed: true, createdAt: 5}),
            issue('i4', {ownerId: 'u7'}),
            issue('i5', {ownerId: 'u1'}),
          ],
        },
        3,
      ),
      advance(10),
    ],
  },
  {
    name: 'aggregate: synced aggregate rows and pending moves survive a restart',
    steps: [
      ...synced('aggregates.commentCounts'),
      addComment('m1', 'c5', 'i2'),
      // Long enough for JS to persist.
      advance(2000),
      {op: 'restart'},
      {op: 'materialize', view: 'a', query: 'aggregates.commentCounts'},
      {op: 'materialize', view: 'b', query: 'aggregates.issueCount'},
      advance(10),
    ],
  },
  {
    name: 'aggregate: once the server drops the query, a mutation no longer moves its rows',
    steps: [
      ...synced('aggregates.commentCounts'),
      {op: 'destroy', view: 'v'},
      advance(10),
      ...poke('p2', '01', '02', {
        gotQueriesPatch: [gotDel(queryHash('aggregates.commentCounts'))],
        rowsPatch: serverRowsPatch(
          'aggregates.commentCounts',
          undefined,
          data,
          {},
        ),
      }),
      addComment('m1', 'c5', 'i1'),
      advance(10),
      {op: 'materialize', view: 'w', query: 'aggregates.commentCounts'},
      advance(10),
    ],
  },
  {
    name: 'aggregate: a mutator reads a synced aggregate with its own writes, and computes one the server has not sent',
    steps: [
      ...synced('aggregates.commentCounts'),
      ops(
        'm1',
        [
          {
            op: 'insert',
            table: 'comment',
            value: {
              id: 'c5',
              authorId: 'u1',
              issueId: 'i1',
              text: '',
              createdAt: 1,
            },
          },
        ],
        [
          {name: 'aggregates.commentCounts'},
          {name: 'aggregates.issueCount'},
          {name: 'aggregates.commentsByIssue'},
        ],
      ),
      advance(10),
    ],
  },
];

type QueryRef = {name: string; args?: ReadonlyJSONValue};

/**
 * Materializes `refs` as views `v0`, `v1`, ..., and the users as `users`
 * (where `aggregates.record` writes what each run read), connects, and
 * syncs the queries' rows over `over` in one poke, with `extra` rows.
 */
function syncedViews(
  refs: QueryRef[],
  over: ServerData,
  extra: ReadonlyJSONValue[] = [],
): Step[] {
  return [
    start,
    ...refs.map((r, i): Step => ({
      op: 'materialize',
      view: `v${i}`,
      query: r.name,
      ...(r.args === undefined ? {} : {args: r.args}),
    })),
    {op: 'materialize', view: 'users', query: 'users.all'},
    connected,
    advance(10),
    ...syncPoke(
      'p1',
      null,
      '01',
      refs.map(r => queryHash(r.name, r.args)),
      [
        ...refs.flatMap(r => serverRowsPatch(r.name, r.args, undefined, over)),
        ...extra,
      ],
    ),
  ];
}

/** The rows the server moves for `refs` from `before` to `after`. */
const moved = (
  refs: QueryRef[],
  before: ServerData,
  after: ServerData,
  version: string,
) => refs.flatMap(r => serverRowsPatch(r.name, r.args, before, after, version));

/**
 * `aggregates.record`: reads `refs`, runs `o`, reads them again, and records
 * both reads, and the reason of the run, in the user `record`.
 */
const record = (
  mutation: string,
  refs: QueryRef[],
  o: {op: string; table: string; value: ReadonlyJSONValue}[],
  rec = mutation,
) =>
  mutate(mutation, 'aggregates.record', {
    queries: refs,
    ops: o,
    record: rec,
  });

const insertIssue = (
  id: string,
  fields: {closed?: boolean; ownerId?: string | null; createdAt?: number},
) => ({
  op: 'insert',
  table: 'issue',
  value: {
    id,
    title: id,
    description: '',
    closed: fields.closed ?? false,
    ownerId: fields.ownerId ?? null,
    createdAt: fields.createdAt ?? 1,
  },
});

const users: ServerData = {
  users: [user('u1', 'Alice'), user('u2', 'Bob')],
  issues: [
    issue('i1', {ownerId: 'u1'}),
    issue('i2'),
    issue('i3', {ownerId: 'u1', closed: true}),
  ],
};

const points: ServerData = {
  users: [
    user('u1', 'A', {points: 5}),
    // A string leaf, another key, and no metadata contribute nothing.
    user('u2', 'A', {points: '7'}),
    user('u3', 'B', {other: 1}),
    user('u4', 'C', null),
  ],
};

const commentCount: QueryRef = {
  name: 'aggregates.issueWithCommentCount',
  args: 'i1',
};
const openByState: QueryRef = {name: 'aggregates.openByState'};
const issueStates: QueryRef = {name: 'aggregates.issueStates'};
const timeByState: QueryRef = {name: 'aggregates.timeByState'};
const avgTimeByState: QueryRef = {name: 'aggregates.avgTimeByState'};

/**
 * Ports of zero-client's tests of synced aggregates
 * (aggregate-read-your-writes, group-read-your-writes, crud-impl, custom,
 * context, zero-poke-handler): each names the test it comes from.
 */
export const upstreamAggregateScenarios: Scenario[] = [
  ...(['optimistic', 'rebase'] as const).map((reason): Scenario => ({
    // aggregate-read-your-writes: 'a custom mutator reads its optimistic
    // aggregate write during %s'.
    name: `aggregate: a mutator reads its optimistic relationship count during ${reason}`,
    steps: [
      // Only the count is synced, not the comments it counts.
      ...syncedViews([commentCount], data),
      record(
        'm1',
        [commentCount],
        [
          {
            op: 'insert',
            table: 'comment',
            value: {
              id: 'c9',
              authorId: 'u1',
              issueId: 'i1',
              text: '',
              createdAt: 1,
            },
          },
        ],
      ),
      advance(10),
      ...(reason === 'rebase'
        ? // The server counts more comments without the pending one: the
          // mutation is replayed over the new count.
          poke('p2', '01', '02', {
            rowsPatch: moved(
              [commentCount],
              data,
              plus(
                'comments',
                comment('c5', 'i1'),
                comment('c6', 'i1'),
                comment('c7', 'i1'),
              ),
              '02',
            ),
          })
        : []),
      advance(10),
    ],
  })),
  ...(['optimistic', 'rebase'] as const).map((reason): Scenario => ({
    // group-read-your-writes: 'a mutator reads its own writes in a synced
    // groupBy during %s'.
    name: `aggregate: a mutator reads its own writes in a synced groupBy during ${reason}`,
    steps: [
      // Only the groups are synced, not the issues they count.
      ...syncedViews([openByState], data),
      // Only the open issues are counted, so a closed one moves nothing.
      record(
        'm1',
        [openByState],
        [insertIssue('i9', {}), insertIssue('i10', {closed: true})],
      ),
      advance(10),
      ...(reason === 'rebase'
        ? poke('p2', '01', '02', {
            rowsPatch: moved(
              [openByState],
              data,
              plus('issues', issue('i4'), issue('i5'), issue('i6')),
              '02',
            ),
          })
        : []),
      advance(10),
    ],
  })),
  {
    // group-read-your-writes: 'a mutator reads its new and removed values
    // in a synced distinct'.
    name: 'aggregate: a mutator reads its new and removed values in a synced distinct',
    steps: [
      // The open value has one issue, i1, which the client also has.
      ...syncedViews([issueStates], {issues: [issue('i1')]}, [
        put('issues', issue('i1')),
      ]),
      record('m1', [issueStates], [insertIssue('i9', {closed: true})]),
      record(
        'm2',
        [issueStates],
        [{op: 'delete', table: 'issue', value: {id: 'i1'}}],
      ),
      advance(10),
    ],
  },
  ...(['sum', 'avg'] as const).map((fn): Scenario => ({
    // group-read-your-writes: 'a mutator reads its own writes in a synced
    // groupBy().%s()'.
    name: `aggregate: a mutator reads its own writes in a synced groupBy().${fn}()`,
    steps: [
      // Two open issues whose times add up to 3.
      ...syncedViews([fn === 'sum' ? timeByState : avgTimeByState], {
        issues: [issue('i1', {createdAt: 1}), issue('i2', {createdAt: 2})],
      }),
      // The first row of the closed group, which isn't synced, adds it.
      record(
        'm1',
        [fn === 'sum' ? timeByState : avgTimeByState],
        [
          insertIssue('i9', {createdAt: 7}),
          insertIssue('i10', {closed: true, createdAt: 4}),
        ],
      ),
      advance(10),
    ],
  })),
  {
    // group-read-your-writes: 'an optimistic count moves the synced group an
    // orderBy reorders'.
    name: 'aggregate: an optimistic count moves the synced group an orderBy reorders',
    steps: [
      // Ordered by owner first: the server keys a group [owner, closed].
      ...syncedViews([{name: 'aggregates.issuesByOwner'}], {
        issues: [issue('i1', {ownerId: 'u1'}), issue('i2', {ownerId: 'u1'})],
      }),
      mutate('m1', 'issue.create', {id: 'i9', ownerId: 'u1'}),
      advance(10),
    ],
  },
  {
    // group-read-your-writes: 'an edit within a group the client lacks adds
    // no group'; crud-impl: 'optimistic grouped sum: a group the client
    // lacks only comes with a new row'.
    name: 'aggregate: an edit within a group the client lacks adds no group, a new row does',
    steps: [
      start,
      {op: 'materialize', view: 'states', query: 'aggregates.issueStates'},
      {op: 'materialize', view: 'times', query: 'aggregates.timeByState'},
      connected,
      advance(10),
      // i1 is synced, by another query, before the server answers these, so
      // its group isn't on the client.
      ...poke('p1', null, '01', {rowsPatch: [put('issues', issue('i1'))]}),
      advance(10),
      mutate('m1', 'issue.update', {id: 'i1', title: 'renamed'}),
      mutate('m2', 'issue.update', {id: 'i1', createdAt: 7}),
      advance(10),
      mutate('m3', 'issue.create', {id: 'i9', closed: true, createdAt: 4}),
      advance(10),
    ],
  },
  {
    // crud-impl: 'optimistic grouped sum: a row stored without its row count
    // is never removed'. The closed group comes from an older server, without
    // `_0_rows`.
    name: 'aggregate: a grouped sum stored without its row count keeps its group when emptied',
    steps: [
      ...syncedViews([], data, [
        ...serverRowsPatch(timeByState.name, undefined, undefined, data).map(
          r => {
            const row = r as {value: Record<string, ReadonlyJSONValue>};
            const {['_0_rows']: _, ...value} = row.value;
            return {...row, value} as ReadonlyJSONValue;
          },
        ),
        ...(data.issues ?? []).map(r => put('issues', r)),
      ]),
      {op: 'materialize', view: 'times', query: timeByState.name},
      ...poke('p2', '01', '02', {
        gotQueriesPatch: [{op: 'put', hash: queryHash(timeByState.name)}],
      }),
      advance(10),
      mutate('m1', 'issue.delete', 'i3'),
      advance(10),
    ],
  },
  {
    // crud-impl: 'optimistic count: no row for the parent', 'a child with a
    // null correlation value has no parent', 'the aggregate row is keyed by
    // its server column names' (owner_id).
    name: 'aggregate: a child with no parent row, or a null correlation, moves no count',
    steps: [
      ...syncedViews([{name: 'aggregates.userIssueCounts'}], users),
      mutate('m1', 'issue.create', {id: 'i8', ownerId: null}),
      mutate('m2', 'issue.create', {id: 'i9', ownerId: 'u2'}),
      mutate('m3', 'issue.update', {id: 'i9', ownerId: null}),
      mutate('m4', 'issue.create', {id: 'i10', ownerId: 'u7'}),
      advance(10),
      // The user the count had no row for arrives.
      mutate('m5', 'aggregates.read', {
        ops: [{op: 'insert', table: 'user', value: {id: 'u7', name: 'Gus'}}],
        queries: [],
      }),
      advance(10),
    ],
  },
  ...(['pointsByName', 'avgPointsByName'] as const).map((name): Scenario => ({
    // crud-impl: 'optimistic sum over a JSON path: only the number leaf
    // moves the sum', 'optimistic sum: null fields contribute 0', 'null
    // over no contributors, 0 over a 0', 'optimistic avg: a null field
    // contributes nothing', 'deleting the last contributor → null',
    // 'optimistic grouped sum: a group comes with its first row and goes
    // with its last'.
    name: `aggregate: a grouped ${name === 'pointsByName' ? 'sum' : 'average'} of a JSON path moves only with number leaves`,
    steps: [
      // The users are synced too, so editing and deleting them applies.
      ...syncedViews(
        [{name: `aggregates.${name}`}],
        points,
        (points.users ?? []).map(u => put('users', u)),
      ),
      ...(
        [
          ['update', {id: 'u1', metadata: {points: 5, note: 'x'}}],
          ['update', {id: 'u2', metadata: {points: 6}}],
          ['insert', {id: 'u5', name: 'A', metadata: {points: 0}}],
          ['delete', {id: 'u1'}],
          ['insert', {id: 'u6', name: 'B', metadata: {points: 0}}],
          ['delete', {id: 'u6'}],
          ['delete', {id: 'u4'}],
          ['insert', {id: 'u7', name: 'D', metadata: {points: 3}}],
          ['insert', {id: 'u8', name: 'D', metadata: null}],
        ] as const
      ).map(([op, value], i) =>
        mutate(`m${i + 1}`, 'aggregates.read', {
          ops: [{op, table: 'user', value}],
          queries: [],
        }),
      ),
      advance(10),
    ],
  })),
  {
    // context: 'synced junction (many-to-many) aggregate (sum) reads the
    // server value', 'synced junction (many-to-many) count with a where
    // reads the server value'. A junction's aggregate is the server's.
    name: "aggregate: a junction max and a junction count with a where are the server's",
    steps: [
      ...syncedViews(
        [{name: 'aggregates.labelMax'}, {name: 'aggregates.bugLabelCounts'}],
        data,
      ),
      ops('m1', [
        {
          op: 'insert',
          table: 'issueLabel',
          value: {issueId: 'i2', labelId: 'l1'},
        },
      ]),
      advance(10),
      ...poke('p2', '01', '02', {
        lastMutationIDChanges: {$clientID: 1},
        rowsPatch: moved(
          [{name: 'aggregates.labelMax'}, {name: 'aggregates.bugLabelCounts'}],
          data,
          plus('issueLabel', {issueId: 'i2', labelId: 'l1'}),
          '02',
        ),
      }),
      advance(10),
    ],
  },
  {
    // custom: 'custom mutators compute aggregates from the rows they can
    // read': a top-level aggregate, a distinct, a groupBy and a distinct
    // reading the mutator's own writes, a relationship aggregate.
    name: 'aggregate: a mutator computes aggregates the server has not sent from its own writes',
    steps: [
      start,
      connected,
      advance(10),
      mutate('m1', 'aggregates.record', {
        queries: [
          'issueCount',
          'latestIssueTime',
          'openIssueCount',
          'issueStates',
          'issueStateCount',
          'countByState',
          'timeByState',
          'commentAndLabelCounts',
          'commentTimeSums',
        ].map(n => ({name: `aggregates.${n}`})),
        ops: [
          insertIssue('1', {}),
          insertIssue('2', {closed: true, createdAt: 5}),
          insertIssue('3', {createdAt: 2}),
          ...[
            ['c1', 2],
            ['c2', 3],
          ].map(([id, createdAt]) => ({
            op: 'insert',
            table: 'comment',
            value: {id, authorId: 'u1', issueId: '1', text: '', createdAt},
          })),
          {op: 'insert', table: 'label', value: {id: 'l1', name: 'bug'}},
          {
            op: 'insert',
            table: 'issueLabel',
            value: {issueId: '1', labelId: 'l1'},
          },
          // An edge to a label that isn't there doesn't count.
          {
            op: 'insert',
            table: 'issueLabel',
            value: {issueId: '2', labelId: 'l2'},
          },
        ],
      }),
      advance(10),
    ],
  },
  {
    // ivm-branch: 'advance releases an aggregate source the server empties
    // once no view reads it'. A grouped query's registration, left behind,
    // would make a new row add a group nothing synced.
    name: 'aggregate: once the server drops a groupBy, a new row adds no group',
    steps: [
      ...syncedViews([{name: 'aggregates.countByState'}], data),
      {op: 'destroy', view: 'v0'},
      advance(10),
      ...poke('p2', '01', '02', {
        gotQueriesPatch: [gotDel(queryHash('aggregates.countByState'))],
        rowsPatch: serverRowsPatch(
          'aggregates.countByState',
          undefined,
          data,
          {},
        ),
      }),
      mutate('m1', 'issue.create', {id: 'i9', closed: true}),
      advance(10),
      {op: 'materialize', view: 'w', query: 'aggregates.countByState'},
      advance(10),
    ],
  },
  {
    // ivm-branch: 'only the main branch registers optimistic aggregates'. A
    // mutator reads a synced groupBy that no view reads and that has no
    // rows, so its source exists only in the mutator's fork.
    name: "aggregate: a mutator's read of a synced groupBy registers nothing",
    steps: [
      ...syncedViews([{name: 'aggregates.countByState'}], {}),
      {op: 'destroy', view: 'v0'},
      advance(10),
      ops('m1', [], [{name: 'aggregates.countByState'}]),
      mutate('m2', 'issue.create', {id: 'i9', closed: true}),
      advance(10),
      {op: 'materialize', view: 'w', query: 'aggregates.countByState'},
      advance(10),
    ],
  },
  {
    // A mutation made after a restart, before any view of the synced
    // groupBy registers it, moves nothing until the next rebase.
    name: 'aggregate: after a restart, a mutation made before the view moves its group at the next rebase',
    steps: [
      ...syncedViews([{name: 'aggregates.countByState'}], data),
      advance(2000),
      {op: 'restart'},
      mutate('m1', 'issue.create', {id: 'i9', closed: true}),
      advance(10),
      {op: 'materialize', view: 'w', query: 'aggregates.countByState'},
      start,
      connected,
      // A poke that confirms nothing replays the pending mutation.
      ...poke('p2', '01', '02', {rowsPatch: [put('issues', issue('i8'))]}),
      advance(10),
    ],
  },
  {
    // zero-poke-handler: 'mergePokes routes synced top-level aggregate rows
    // (not dropped)', 'mergePokes routes synced relationship aggregate rows'.
    name: 'aggregate: pokes put and delete aggregate rows, and drop a table not in the schema',
    steps: [
      ...syncedViews(
        [{name: 'aggregates.issueCount'}, {name: 'aggregates.commentCounts'}],
        data,
        [put('notInSchema', {id: 'x'})],
      ),
      advance(10),
      ...poke('p2', '01', '02', {
        rowsPatch: [
          del(`aggregate:${queryHash('aggregates.issueCount')}`, {_0_key: 0}),
          del(`aggregate:${queryHash('aggregates.commentCounts')}:comments`, {
            ['issue_id']: 'i1',
          }),
        ],
      }),
      advance(10),
    ],
  },
];
