/**
 * Aggregate bug hunt: targeted client scenarios for synced aggregates. The
 * server rows come from JS's pipeline (`server-rows.ts`).
 */
import type {ReadonlyJSONValue} from '../../../../shared/src/json.ts';
import type {Scenario, Step} from '../harness.ts';
import {serverRowsPatch, type ServerData} from '../server-rows.ts';
import {
  advance,
  comment,
  connected,
  del,
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
  ],
  users: [user('u1', 'A', {points: 1}), user('u2', 'B', {points: 2})],
};

const rows = (name: string, over: ServerData = data, version = '01') =>
  serverRowsPatch(name, undefined, undefined, over, version);

/** Views of `names` (`v0`, ...) and of all comments and issues, synced. */
function synced(names: string[], over: ServerData = data): Step[] {
  return [
    start,
    ...names.map((query, i): Step => ({
      op: 'materialize',
      view: `v${i}`,
      query,
    })),
    {op: 'materialize', view: 'comments', query: 'bughunt.allComments'},
    {op: 'materialize', view: 'issues', query: 'issues.all'},
    connected,
    advance(10),
    ...syncPoke(
      'p1',
      null,
      '01',
      [...names, 'bughunt.allComments', 'issues.all'].map(n => queryHash(n)),
      [
        ...names.flatMap(n => rows(n, over)),
        ...(over['comments'] ?? []).map(c => put('comments', c)),
        ...(over['issues'] ?? []).map(c => put('issues', c)),
      ],
    ),
    advance(10),
  ];
}

const ops = (
  mutation: string,
  o: {op: string; table: string; value: ReadonlyJSONValue}[],
  extra: Record<string, ReadonlyJSONValue> = {},
) => mutate(mutation, 'harden.ops', {ops: o, ...extra});

const newComment = (id: string, issueId: string, createdAt = 1) => ({
  op: 'insert',
  table: 'comment',
  value: {id, authorId: 'u1', issueId, text: '', createdAt},
});

const countRow = (value: number) => ({
  _0_key: 0,
  _0_value: value,
  _0_version: '02',
});
const issueCountTable = () => `aggregate:${queryHash('aggregates.issueCount')}`;
const commentCountTable = () =>
  `aggregate:${queryHash('aggregates.commentCounts')}:comments`;

export const bughuntScenarios: Scenario[] = [
  {
    name: 'bughunt: a poke puts and then deletes a top-level aggregate row',
    steps: [
      ...synced(['aggregates.issueCount']),
      ...poke('p2', '01', '02', {
        rowsPatch: [
          put(issueCountTable(), countRow(9)),
          del(issueCountTable(), {_0_key: 0}),
        ],
      }),
      advance(10),
    ],
  },
  {
    name: 'bughunt: a poke deletes and then puts a top-level aggregate row',
    steps: [
      ...synced(['aggregates.issueCount']),
      ...poke('p2', '01', '02', {
        rowsPatch: [
          del(issueCountTable(), {_0_key: 0}),
          put(issueCountTable(), countRow(9)),
        ],
      }),
      advance(10),
    ],
  },
  {
    name: 'bughunt: a poke deletes and puts a relationship aggregate row, then updates it',
    steps: [
      ...synced(['aggregates.commentCounts']),
      ...poke('p2', '01', '02', {
        rowsPatch: [
          del(commentCountTable(), {issue_id: 'i1'}),
          put(commentCountTable(), {
            issue_id: 'i1',
            _0_value: 5,
            _0_version: '02',
          }),
          {
            op: 'update',
            tableName: commentCountTable(),
            id: {issue_id: 'i2'},
            merge: {_0_value: 7},
          },
        ],
      }),
      advance(10),
      ops('m1', [newComment('c9', 'i1'), newComment('c8', 'i2')]),
      advance(10),
    ],
  },
  {
    name: 'bughunt: a poke clears the rows and puts them again',
    steps: [
      ...synced(['aggregates.issueCount', 'aggregates.commentCounts']),
      ...poke('p2', '01', '02', {
        rowsPatch: [
          {op: 'clear'},
          ...rows('aggregates.commentCounts', data, '02'),
          ...(data['issues'] ?? []).map(c => put('issues', c)),
        ],
      }),
      advance(10),
      ops('m1', [newComment('c9', 'i1')]),
      advance(10),
    ],
  },
  {
    name: 'bughunt: rows of a query the client never asked for, then its view',
    steps: [
      ...synced([]),
      ...poke('p2', '01', '02', {
        rowsPatch: rows('aggregates.commentCounts', data, '02'),
      }),
      advance(10),
      ops('m1', [newComment('c9', 'i1')]),
      advance(10),
      {op: 'materialize', view: 'late', query: 'aggregates.commentCounts'},
      advance(10),
      ops('m2', [newComment('c8', 'i1')]),
      advance(10),
      advance(2000),
      {op: 'restart'},
      {op: 'materialize', view: 'again', query: 'aggregates.commentCounts'},
      advance(10),
    ],
  },
  {
    name: 'bughunt: a mutator that moves aggregates and then throws moves nothing',
    steps: [
      ...synced([
        'aggregates.commentCounts',
        'aggregates.commentsByIssue',
        'aggregates.timeByAuthor',
      ]),
      ops('m1', [newComment('c9', 'i1'), newComment('c8', 'i9')], {
        fail: 'boom',
      }),
      advance(10),
      ops('m2', [{op: 'delete', table: 'comment', value: {id: 'c3'}}], {
        fail: 'boom',
      }),
      advance(10),
    ],
  },
  {
    name: 'bughunt: a group emptied and refilled in one mutation',
    steps: [
      ...synced([
        'aggregates.commentsByIssue',
        'aggregates.timeByAuthor',
        'bughunt.commentsByAuthorAndIssue',
        'bughunt.avgTimeByIssueWhere',
      ]),
      ops('m1', [
        {op: 'delete', table: 'comment', value: {id: 'c3'}},
        newComment('c9', 'i2', 8),
      ]),
      advance(10),
      ops('m2', [
        {op: 'update', table: 'comment', value: {id: 'c9', issueId: 'i7'}},
        {op: 'update', table: 'comment', value: {id: 'c9', issueId: 'i2'}},
      ]),
      advance(10),
      ops('m3', [
        {op: 'delete', table: 'comment', value: {id: 'c9'}},
        {op: 'delete', table: 'comment', value: {id: 'c1'}},
        {op: 'delete', table: 'comment', value: {id: 'c2'}},
      ]),
      advance(10),
    ],
  },
  {
    name: 'bughunt: rebases of several pending mutations over moved aggregates',
    steps: [
      ...synced([
        'aggregates.commentCounts',
        'aggregates.commentTimes',
        'aggregates.commentsByIssue',
        'aggregates.issueCount',
      ]),
      ops('m1', [newComment('c9', 'i1', 9)]),
      ops('m2', [{op: 'delete', table: 'comment', value: {id: 'c3'}}]),
      ops('m3', [
        {op: 'update', table: 'comment', value: {id: 'c1', issueId: 'i3'}},
      ]),
      advance(10),
      // Another client adds a comment to i2.
      ...poke('p2', '01', '02', {
        rowsPatch: [
          ...serverRowsPatch(
            'aggregates.commentCounts',
            undefined,
            data,
            {...data, comments: [...data['comments'], comment('x1', 'i2')]},
            '02',
          ),
          ...serverRowsPatch(
            'aggregates.commentsByIssue',
            undefined,
            data,
            {...data, comments: [...data['comments'], comment('x1', 'i2')]},
            '02',
          ),
          put('comments', comment('x1', 'i2')),
        ],
      }),
      advance(10),
      // The server confirms m1 only.
      ...poke('p3', '02', '03', {
        lastMutationIDChanges: {$clientID: 1},
        rowsPatch: [
          ...serverRowsPatch(
            'aggregates.commentCounts',
            undefined,
            {...data, comments: [...data['comments'], comment('x1', 'i2')]},
            {
              ...data,
              comments: [
                ...data['comments'],
                comment('x1', 'i2'),
                comment('c9', 'i1', {createdAt: 9}),
              ],
            },
            '03',
          ),
          put('comments', comment('c9', 'i1', {createdAt: 9})),
        ],
      }),
      advance(2000),
      {op: 'restart'},
      {op: 'materialize', view: 'v0', query: 'aggregates.commentCounts'},
      {op: 'materialize', view: 'v2', query: 'aggregates.commentsByIssue'},
      advance(10),
    ],
  },
  {
    // Swift's rebase when an aggregate first registers after a restart
    // (AGGREGATES-PLAN §6) replays every pending mutation, also one made
    // after the launch, which JS moves only at its next rebase.
    name: 'bughunt: after a restart, registering a groupBy moves it by a mutation made since',
    steps: [
      start,
      connected,
      advance(10),
      mutate('m1', 'issue.create', {id: 'i9'}),
      advance(2000),
      {op: 'restart'},
      start,
      connected,
      advance(10),
      mutate('m2', 'comment.add', {id: 'c9', issueId: 'i1'}),
      advance(10),
      {op: 'materialize', view: 'v', query: 'aggregates.commentsByIssue'},
      advance(10),
    ],
  },
  {
    // The same for a synced relationship count: the server's count is
    // stored, and a comment added after the launch moves it at once.
    name: 'bughunt: after a restart, registering a relationship count moves it by a mutation made since',
    steps: [
      ...synced(['aggregates.commentCounts']),
      mutate('m1', 'issue.create', {id: 'i9'}),
      advance(2000),
      {op: 'restart'},
      start,
      connected,
      advance(10),
      mutate('m2', 'comment.add', {id: 'c9', issueId: 'i1'}),
      advance(10),
      {op: 'materialize', view: 'v', query: 'aggregates.commentCounts'},
      advance(10),
    ],
  },
  {
    // The same rebase replays a mutation from before the restart into a
    // query that wasn't registered when it ran: JS's persisted write never
    // moved it.
    name: 'bughunt: after a restart, a groupBy first materialized is moved by a mutation from before',
    steps: [
      start,
      connected,
      advance(10),
      mutate('m1', 'issue.create', {id: 'i9', closed: true}),
      advance(2000),
      {op: 'restart'},
      start,
      connected,
      advance(10),
      {op: 'materialize', view: 'v', query: 'aggregates.countByState'},
      advance(10),
    ],
  },
  {
    name: 'bughunt: a sum over a JSON path and a nested relationship count move optimistically',
    steps: [
      ...synced(['bughunt.pointsSum', 'bughunt.ownerIssueCounts']),
      ops('m1', [
        {
          op: 'insert',
          table: 'user',
          value: {id: 'u3', name: 'C', metadata: {points: 4}},
        },
        {
          op: 'insert',
          table: 'issue',
          value: {
            id: 'i9',
            title: '',
            description: '',
            closed: false,
            ownerId: 'u1',
            createdAt: 1,
          },
        },
      ]),
      advance(10),
    ],
  },
];
