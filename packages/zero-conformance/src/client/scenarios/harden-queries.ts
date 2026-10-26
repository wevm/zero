/**
 * Hardening scenarios for views and queries: listeners that re-enter the
 * client (remove or add listeners, destroy views, materialize, mutate) from
 * inside a notification, TTL strings JS parses oddly, query change
 * throttling, and pokes that do not change what a view shows.
 *
 * Emitted to `vectors/harden-queries.json` by `harden-queries.test.ts`.
 */
import type {Scenario, Step} from '../harness.ts';
import {
  advance,
  connected,
  del,
  issue,
  mutate,
  poke,
  put,
  queryHash,
  start,
  syncPoke,
} from './helpers.ts';

const allHash = queryHash('issues.all');
const openHash = queryHash('issues.open');
const byIDi1 = queryHash('issues.byID', 'i1');

const listen = (
  view: string,
  listener: string,
  actions?: Step[],
  after?: number,
): Step => ({
  op: 'listen',
  view,
  listener,
  ...(actions ? {actions} : {}),
  ...(after !== undefined ? {after} : {}),
});

/** Connected, with `issues.all` in view `v` and complete with no rows. */
const allView: Step[] = [
  start,
  connected,
  {op: 'materialize', view: 'v', query: 'issues.all'},
  advance(10),
  ...syncPoke('p0', null, '00', [allHash]),
];

export const hardenQueryScenarios: Scenario[] = [
  {
    name: 'listener: removing a later listener during a notification skips it',
    steps: [
      ...allView,
      listen('v', 'L1', [{op: 'unlisten', listener: 'L2'}]),
      listen('v', 'L2'),
      ...syncPoke('p1', '00', '01', [], [put('issues', issue('i1'))]),
      ...syncPoke('p2', '01', '02', [], [put('issues', issue('i2'))]),
    ],
  },
  {
    name: 'listener: removing an earlier listener during a notification',
    steps: [
      ...allView,
      listen('v', 'L1'),
      listen('v', 'L2', [{op: 'unlisten', listener: 'L1'}]),
      listen('v', 'L3'),
      ...syncPoke('p1', '00', '01', [], [put('issues', issue('i1'))]),
      ...syncPoke('p2', '01', '02', [], [put('issues', issue('i2'))]),
    ],
  },
  {
    name: 'listener: adding a listener during a notification',
    steps: [
      ...allView,
      listen('v', 'L1', [listen('v', 'L2')]),
      ...syncPoke('p1', '00', '01', [], [put('issues', issue('i1'))]),
      ...syncPoke('p2', '01', '02', [], [put('issues', issue('i2'))]),
    ],
  },
  {
    name: 'listener: a listener that destroys its own view',
    steps: [
      ...allView,
      listen('v', 'L1', [{op: 'destroy', view: 'v'}]),
      listen('v', 'L2'),
      ...syncPoke('p1', '00', '01', [], [put('issues', issue('i1'))]),
      advance(10),
      ...syncPoke('p2', '01', '02', [], [put('issues', issue('i2'))]),
    ],
  },
  {
    name: 'listener: a mutation from a listener, with a later listener',
    steps: [
      ...allView,
      listen('v', 'L1', [mutate('m2', 'issue.create', {id: 'i2'})]),
      listen('v', 'L2'),
      mutate('m1', 'issue.create', {id: 'i1'}),
      advance(10),
    ],
  },
  {
    name: 'listener: a mutation from the listener of one view, seen by another',
    steps: [
      ...allView,
      {op: 'materialize', view: 'w', query: 'issues.all'},
      listen('v', 'L1', [mutate('m2', 'issue.create', {id: 'i2'})]),
      listen('w', 'L2'),
      mutate('m1', 'issue.create', {id: 'i1'}),
      advance(10),
    ],
  },
  {
    name: 'listener: a poke while a listener mutates',
    steps: [
      ...allView,
      listen('v', 'L1', [mutate('m1', 'issue.create', {id: 'i9'})]),
      listen('v', 'L2'),
      ...syncPoke('p1', '00', '01', [], [put('issues', issue('i1'))]),
      advance(10),
    ],
  },
  {
    name: 'listener: materializing the same query from a listener',
    steps: [
      ...allView,
      listen('v', 'L1', [
        {op: 'materialize', view: 'w', query: 'issues.all'},
        listen('w', 'L2'),
      ]),
      ...syncPoke('p1', '00', '01', [], [put('issues', issue('i1'))]),
      ...syncPoke('p2', '01', '02', [], [put('issues', issue('i2'))]),
      advance(10),
    ],
  },
  {
    name: 'listener: materializing another query from a listener',
    steps: [
      ...allView,
      listen('v', 'L1', [
        {op: 'materialize', view: 'w', query: 'issues.byID', args: 'i1'},
        listen('w', 'L2'),
      ]),
      ...syncPoke('p1', '00', '01', [], [put('issues', issue('i1'))]),
      advance(10),
      ...syncPoke('p2', '01', '02', [byIDi1], [del('issues', {id: 'i1'})]),
    ],
  },
  {
    name: 'listener: destroying another view of the same commit',
    steps: [
      start,
      connected,
      {op: 'materialize', view: 'a', query: 'issues.all'},
      {op: 'materialize', view: 'b', query: 'issues.open'},
      advance(10),
      listen('a', 'La', [{op: 'destroy', view: 'b'}]),
      listen('b', 'Lb'),
      ...syncPoke(
        'p1',
        null,
        '01',
        [allHash, openHash],
        [put('issues', issue('i1'))],
      ),
      advance(10),
    ],
  },
  {
    name: 'listener: destroying an earlier view of the same commit',
    steps: [
      start,
      connected,
      {op: 'materialize', view: 'a', query: 'issues.all'},
      {op: 'materialize', view: 'b', query: 'issues.open'},
      advance(10),
      listen('a', 'La'),
      listen('b', 'Lb', [{op: 'destroy', view: 'a'}]),
      ...syncPoke(
        'p1',
        null,
        '01',
        [allHash, openHash],
        [put('issues', issue('i1'))],
      ),
      advance(10),
    ],
  },
  {
    name: 'listener: updateTTL and run from a listener',
    steps: [
      ...allView,
      listen('v', 'L1', [
        {op: 'updateTTL', view: 'v', ttl: '10m'},
        {op: 'run', run: 'r', query: 'issues.all'},
      ]),
      ...syncPoke('p1', '00', '01', [], [put('issues', issue('i1'))]),
      advance(10),
    ],
  },
  {
    name: 'listener: preload and cleanup from a listener',
    steps: [
      ...allView,
      listen('v', 'L1', [
        {op: 'preload', preload: 'p', query: 'users.all'},
        {op: 'cleanup', preload: 'p'},
        {op: 'cleanup', preload: 'p'},
      ]),
      ...syncPoke('p1', '00', '01', [], [put('issues', issue('i1'))]),
      advance(10),
    ],
  },
  {
    name: 'view: a poke that rewrites a row with the same values',
    steps: [
      ...allView,
      ...syncPoke('p1', '00', '01', [], [put('issues', issue('i1'))]),
      ...syncPoke('p2', '01', '02', [], [put('issues', issue('i1'))]),
    ],
  },
  {
    name: 'view: a poke that removes a row and puts it back',
    steps: [
      ...allView,
      ...syncPoke('p1', '00', '01', [], [put('issues', issue('i1'))]),
      ...syncPoke(
        'p2',
        '01',
        '02',
        [],
        [del('issues', {id: 'i1'}), put('issues', issue('i1'))],
      ),
      ...syncPoke(
        'p3',
        '02',
        '03',
        [],
        [
          del('issues', {id: 'i1'}),
          put('issues', issue('i1', {title: 'changed'})),
        ],
      ),
    ],
  },
  {
    name: 'view: a poke that changes rows the view does not show',
    steps: [
      start,
      connected,
      {op: 'materialize', view: 'v', query: 'issues.byID', args: 'i1'},
      {op: 'materialize', view: 'w', query: 'issues.open'},
      advance(10),
      ...syncPoke(
        'p1',
        null,
        '01',
        [byIDi1, openHash],
        [put('issues', issue('i1'))],
      ),
      ...syncPoke(
        'p2',
        '01',
        '02',
        [],
        [put('issues', issue('i2', {closed: true}))],
      ),
      ...syncPoke('p3', '02', '03', [], [put('users', {id: 'u9', name: 'x'})]),
    ],
  },
  {
    name: 'view: the poke that confirms an optimistic write with the same row',
    steps: [
      ...allView,
      mutate('m1', 'issue.create', {id: 'i1', title: 'I1'}),
      ...poke('p1', '00', '01', {
        lastMutationIDChanges: {$clientID: 1},
        rowsPatch: [put('issues', issue('i1'))],
      }),
    ],
  },
  {
    name: 'lifecycle: materialize and destroy before the store is loaded',
    steps: [
      {op: 'materialize', view: 'v', query: 'issues.all'},
      {op: 'destroy', view: 'v'},
      start,
      connected,
      advance(10),
    ],
  },
  {
    name: 'lifecycle: a listener added before the store is loaded destroys the view on hydration',
    steps: [
      {op: 'materialize', view: 'v', query: 'issues.all'},
      {op: 'materialize', view: 'w', query: 'issues.all'},
      listen('v', 'L1', [{op: 'destroy', view: 'w'}]),
      start,
      connected,
      advance(10),
      ...syncPoke('p1', null, '01', [allHash], [put('issues', issue('i1'))]),
    ],
  },
  {
    name: 'lifecycle: a listener destroys the other view of its query',
    steps: [
      ...allView,
      {op: 'materialize', view: 'w', query: 'issues.all'},
      listen('v', 'L1', [{op: 'destroy', view: 'w'}]),
      listen('w', 'L2'),
      ...syncPoke('p1', '00', '01', [], [put('issues', issue('i1'))]),
      advance(10),
      {op: 'destroy', view: 'v'},
      advance(10),
    ],
  },
  {
    name: 'lifecycle: one() over a row deleted and put back in one poke',
    steps: [
      start,
      connected,
      {op: 'materialize', view: 'v', query: 'issues.byID', args: 'i1'},
      advance(10),
      ...syncPoke('p1', null, '01', [byIDi1], [put('issues', issue('i1'))]),
      ...syncPoke(
        'p2',
        '01',
        '02',
        [],
        [
          del('issues', {id: 'i1'}),
          put('issues', issue('i1', {ownerId: 'u1'})),
        ],
      ),
      ...syncPoke(
        'p3',
        '02',
        '03',
        [],
        [
          put('issues', issue('i1', {ownerId: 'u2'})),
          del('issues', {id: 'i1'}),
        ],
      ),
    ],
  },
  {
    name: 'lifecycle: materialize, run, and preload after close',
    steps: [
      ...allView,
      ...syncPoke('p1', '00', '01', [], [put('issues', issue('i1'))]),
      {op: 'close'},
      {op: 'materialize', view: 'w', query: 'issues.all'},
      {op: 'run', run: 'r', query: 'issues.all'},
      {op: 'destroy', view: 'w'},
      advance(10),
    ],
  },
  {
    name: 'lifecycle: a preload and a view of one query with different TTLs',
    steps: [
      start,
      connected,
      {op: 'preload', preload: 'p', query: 'issues.all', ttl: '1m'},
      advance(10),
      {op: 'materialize', view: 'v', query: 'issues.all', ttl: '30s'},
      advance(10),
      {op: 'cleanup', preload: 'p'},
      advance(10),
      {op: 'materialize', view: 'w', query: 'issues.all', ttl: '2m'},
      {op: 'destroy', view: 'v'},
      {op: 'destroy', view: 'w'},
      advance(10),
    ],
  },
  {
    name: 'lifecycle: a transformError after the query is complete',
    steps: [
      ...allView,
      {
        op: 'receive',
        message: [
          'transformError',
          [{error: 'app', id: allHash, name: 'issues.all', message: 'late'}],
        ],
      },
      ...syncPoke('p1', '00', '01', [], [put('issues', issue('i1'))]),
      {op: 'run', run: 'r', query: 'issues.all', type: 'complete'},
    ],
  },
  {
    name: 'poke: materialize between pokeStart and pokeEnd',
    steps: [
      ...allView,
      {op: 'receive', message: ['pokeStart', {pokeID: 'p1', baseCookie: '00'}]},
      {
        op: 'receive',
        message: [
          'pokePart',
          {pokeID: 'p1', rowsPatch: [put('issues', issue('i1'))]},
        ],
      },
      {op: 'materialize', view: 'w', query: 'issues.all'},
      {op: 'destroy', view: 'v'},
      {op: 'receive', message: ['pokeEnd', {pokeID: 'p1', cookie: '01'}]},
      advance(10),
    ],
  },
  {
    name: 'poke: a got query deleted while views are complete and cached',
    steps: [
      ...allView,
      {op: 'materialize', view: 'w', query: 'users.all'},
      ...syncPoke('p1', '00', '01', [queryHash('users.all')]),
      {op: 'socketClose', code: 1006},
      {op: 'materialize', view: 'x', query: 'users.all'},
      advance(5000),
      connected,
      ...poke('p2', '01', '02', {
        gotQueriesPatch: [{op: 'del', hash: queryHash('users.all')}],
      }),
      advance(10),
    ],
  },
  {
    name: 'run: complete pending over a dropped connection',
    steps: [
      start,
      connected,
      {op: 'run', run: 'r', query: 'issues.all', type: 'complete'},
      {op: 'run', run: 'c', query: 'issues.all', type: 'cached'},
      advance(10),
      {op: 'socketClose', code: 1006},
      advance(5000),
      connected,
      advance(10),
      ...syncPoke('p1', null, '01', [allHash], [put('issues', issue('i1'))]),
      advance(10),
    ],
  },
  {
    name: 'throttle: the batch timer fires while disconnected',
    steps: [
      start,
      connected,
      {op: 'materialize', view: 'a', query: 'issues.all'},
      {op: 'socketClose', code: 1006},
      advance(10),
      {op: 'materialize', view: 'b', query: 'users.all'},
      advance(10),
      advance(5000),
      connected,
      advance(10),
    ],
  },
  {
    name: 'lifecycle: a removal deferred by a pending mutation over a reconnect',
    steps: [
      ...allView,
      mutate('m1', 'issue.create', {id: 'i1'}),
      {op: 'destroy', view: 'v'},
      advance(10),
      {op: 'socketClose', code: 1006},
      advance(5000),
      connected,
      advance(10),
      ...poke('p1', '00', '01', {lastMutationIDChanges: {$clientID: 1}}),
      advance(10),
    ],
  },
  {
    name: 'error: a query that fails to hydrate throws from materialize and run',
    steps: [
      // Registered first, so the failing calls below don't change what the
      // server is sent (JS leaves their registrations behind; Swift drops
      // them).
      {op: 'preload', preload: 'p', query: 'issues.withFirstComment'},
      ...allView,
      {op: 'materialize', view: 'w', query: 'issues.withFirstComment'},
      {op: 'run', run: 'r', query: 'issues.withFirstComment'},
      {op: 'run', run: 'c', query: 'issues.withFirstComment', type: 'complete'},
      advance(10),
      ...syncPoke('p1', '00', '01', [], [put('issues', issue('i1'))]),
    ],
  },
  {
    name: 'ttl: strings JS parses oddly',
    steps: [
      start,
      connected,
      ...[
        '5x',
        '',
        's',
        'Infinitys',
        '0x10s',
        '1e1m',
        'NaNs',
        '-5m',
        '1_0s',
        ' 3s',
        '3s ',
        '+2m',
        '.5m',
        '5.m',
        '0b11s',
        '1e400s',
        '-0s',
        '5M',
        '1h',
      ].map((ttl, i): Step => ({
        op: 'materialize',
        view: `v${i}`,
        query: 'issues.byOwner',
        args: `t${i}`,
        // oxlint-disable-next-line @typescript-eslint/no-explicit-any
        ttl: ttl as any,
      })),
      advance(10),
    ],
  },
  {
    name: 'ttl: numbers at the edges',
    steps: [
      start,
      connected,
      ...[
        0,
        -1,
        -0.5,
        0.001,
        1e-7,
        599_999.5,
        600_000,
        600_000.5,
        1e21,
        2 ** 53,
      ].map((ttl, i): Step => ({
        op: 'materialize',
        view: `v${i}`,
        query: 'issues.byOwner',
        args: `n${i}`,
        ttl,
      })),
      advance(10),
    ],
  },
  {
    name: 'ttl: updateTTL from an unparsable TTL',
    steps: [
      start,
      connected,
      // oxlint-disable-next-line @typescript-eslint/no-explicit-any
      {op: 'materialize', view: 'v', query: 'issues.all', ttl: '5x' as any},
      advance(10),
      {op: 'updateTTL', view: 'v', ttl: '1m'},
      advance(10),
      {op: 'updateTTL', view: 'v', ttl: 'forever'},
      advance(10),
      {op: 'materialize', view: 'w', query: 'issues.all', ttl: '2m'},
      advance(10),
    ],
  },
  {
    name: 'ttl: updateTTL to an unparsable TTL, none, and a negative string',
    steps: [
      start,
      connected,
      {op: 'materialize', view: 'v', query: 'issues.all', ttl: 'none'},
      advance(10),
      // oxlint-disable-next-line @typescript-eslint/no-explicit-any
      {op: 'updateTTL', view: 'v', ttl: 'abc' as any},
      advance(10),
      {op: 'updateTTL', view: 'v', ttl: '-1m'},
      advance(10),
      {op: 'updateTTL', view: 'v', ttl: -1},
      advance(10),
      {op: 'updateTTL', view: 'v', ttl: 1},
      advance(10),
    ],
  },
  {
    name: 'ttl: preload with odd TTLs',
    steps: [
      start,
      connected,
      // oxlint-disable-next-line @typescript-eslint/no-explicit-any
      {op: 'preload', preload: 'a', query: 'issues.all', ttl: 'x' as any},
      {op: 'preload', preload: 'b', query: 'issues.all', ttl: 'forever'},
      {op: 'preload', preload: 'c', query: 'users.all', ttl: 0},
      advance(10),
      {op: 'cleanup', preload: 'a'},
      {op: 'cleanup', preload: 'b'},
      {op: 'cleanup', preload: 'b'},
      {op: 'cleanup', preload: 'c'},
      advance(10),
    ],
  },
  {
    name: 'throttle: queryChangeThrottleMs 0',
    options: {queryChangeThrottleMs: 0},
    steps: [
      start,
      connected,
      {op: 'materialize', view: 'a', query: 'issues.all'},
      {op: 'materialize', view: 'b', query: 'users.all'},
      {op: 'destroy', view: 'a'},
      advance(1),
      {op: 'materialize', view: 'c', query: 'issues.open'},
    ],
  },
  {
    name: 'throttle: a negative queryChangeThrottleMs',
    options: {queryChangeThrottleMs: -5},
    steps: [
      start,
      connected,
      {op: 'materialize', view: 'a', query: 'issues.all'},
      advance(1),
      {op: 'materialize', view: 'b', query: 'users.all'},
    ],
  },
  {
    name: 'throttle: changes while connecting go in the connect message',
    options: {queryChangeThrottleMs: 50},
    steps: [
      start,
      {op: 'materialize', view: 'a', query: 'issues.all'},
      advance(20),
      connected,
      {op: 'materialize', view: 'b', query: 'users.all'},
      advance(20),
      {op: 'destroy', view: 'a'},
      advance(40),
    ],
  },
];
