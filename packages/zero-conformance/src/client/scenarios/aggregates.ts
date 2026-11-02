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
  gotDel,
  issue,
  mutate,
  poke,
  queryHash,
  start,
  syncPoke,
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
