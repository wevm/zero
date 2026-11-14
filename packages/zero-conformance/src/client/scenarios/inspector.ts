/**
 * The inspector: its RPCs over the socket (metrics, version, queries,
 * analyze, authenticate) and how it resolves the server's answers, its view
 * of the local store, and the query metrics and slow materialization logs.
 */
import type {ReadonlyJSONValue} from '../../../../shared/src/json.ts';
import type {PlanDebugEventJSON} from '../../../../zero-protocol/src/analyze-query-result.ts';
import type {Scenario, Step} from '../harness.ts';
import {
  advance,
  connected,
  issue,
  mutate,
  put,
  queryHash,
  start,
  syncPoke,
  user,
} from './helpers.ts';

const allHash = queryHash('issues.all');
const usersHash = queryHash('users.all');
const byOwnerU1 = queryHash('issues.byOwner', 'u1');

function answer(n: number, op: string, value: ReadonlyJSONValue): Step {
  return {
    op: 'receive',
    message: ['inspect', {op, id: `$inspect.${n}`, value}],
  };
}

const authenticated = (n: number, value: boolean) =>
  answer(n, 'authenticated', value);

const serverMetrics = {
  'query-materialization-server': [100, 5, 1, 7.5, 2, 40, 1],
  'query-update-server': [100, 0.5, 3, 1.25, 1],
};

/** Issue rows i00..i<n-1>, as the server sends them. */
function issues(n: number) {
  return Array.from({length: n}, (_, i) =>
    put('issues', issue(`i${String(i).padStart(2, '0')}`)),
  );
}

const queryRows = [
  {
    clientID: '$clientID',
    queryID: allHash,
    ast: {table: 'issues', orderBy: [['id', 'asc']]},
    name: 'issues.all',
    args: [],
    got: true,
    deleted: false,
    ttl: 300_000,
    inactivatedAt: null,
    rowCount: 40,
    metrics: {
      'query-hydration-server-ms': 12.5,
      'query-update-server': [100, 1, 2, 3, 1, 9, 1],
    },
  },
  {
    clientID: '$clientID',
    queryID: usersHash,
    ast: null,
    name: 'users.all',
    args: [],
    got: true,
    deleted: false,
    ttl: 60_000,
    inactivatedAt: 1_700_000_000_123.5,
    rowCount: 1,
  },
  {
    clientID: 'other-client',
    queryID: 'q-unknown',
    ast: {
      table: 'issues',
      where: {
        type: 'simple',
        op: '=',
        left: {type: 'column', name: 'owner_id'},
        right: {type: 'literal', value: "o'k"},
      },
      limit: 3,
    },
    name: null,
    args: null,
    got: false,
    deleted: true,
    ttl: -1,
    inactivatedAt: null,
    rowCount: 0,
    metrics: {
      'query-hydration-server-ms': 40,
      'query-update-server': [100],
    },
  },
];

const joinPlans: PlanDebugEventJSON[] = [
  {type: 'attempt-start', attemptNumber: 0, totalAttempts: 2},
  {
    type: 'node-cost',
    attemptNumber: 0,
    nodeType: 'connection',
    node: 'issues',
    branchPattern: [0],
    downstreamChildSelectivity: 0.125,
    costEstimate: {
      startupCost: 0,
      scanEst: 100,
      cost: 100.005,
      returnedRows: 10,
      selectivity: 0.1,
      limit: 3,
    },
    filters: {
      type: 'simple',
      op: '=',
      left: {type: 'column', name: 'owner_id'},
      right: {type: 'literal', value: 'u1'},
    },
    ordering: [['id', 'asc']],
  },
  {
    type: 'node-constraint',
    attemptNumber: 0,
    nodeType: 'connection',
    node: 'issues',
    branchPattern: [0],
    constraint: {owner_id: null},
    from: 'join',
  },
  {
    type: 'node-cost',
    attemptNumber: 0,
    nodeType: 'join',
    node: 'join(issues,comments)',
    branchPattern: [],
    downstreamChildSelectivity: 1,
    costEstimate: {
      startupCost: 1,
      scanEst: 2,
      cost: 3.333,
      returnedRows: 4,
      selectivity: 1,
    },
    joinType: 'semi',
  },
  {
    type: 'plan-complete',
    attemptNumber: 0,
    totalCost: 103.338,
    flipPattern: 0,
    joinStates: [{join: 'join(issues,comments)', type: 'semi'}],
  },
  {type: 'attempt-start', attemptNumber: 1, totalAttempts: 2},
  {type: 'plan-failed', attemptNumber: 1, reason: 'unflippable join'},
  {
    type: 'best-plan-selected',
    bestAttemptNumber: 0,
    totalCost: 103.338,
    flipPattern: 0,
    joinStates: [{join: 'join(issues,comments)', type: 'semi'}],
  },
];

/** Shapes `joinPlans` lacks: more attempts, every value kind, no best plan. */
const morePlans: PlanDebugEventJSON[] = [
  {type: 'attempt-start', attemptNumber: 2, totalAttempts: 5},
  {
    type: 'node-cost',
    attemptNumber: 2,
    nodeType: 'connection',
    node: 'comments',
    branchPattern: [1, 0],
    downstreamChildSelectivity: 0.000000125,
    costEstimate: {
      startupCost: 2.675,
      scanEst: 1e21,
      cost: 0.125,
      returnedRows: 1.005,
      selectivity: 0.123456785,
    },
    filters: {
      type: 'and',
      conditions: [
        {
          type: 'simple',
          op: 'IN',
          left: {type: 'column', name: 'id'},
          right: {type: 'literal', value: ['a', 1]},
        },
        {
          type: 'or',
          conditions: [
            {
              type: 'simple',
              op: '>',
              left: {
                type: 'json',
                value: {type: 'column', name: 'm'},
                path: ['a', 0],
              },
              right: {type: 'static', anchor: 'authData', field: ['x', 'y']},
            },
            {
              type: 'correlatedSubquery',
              op: 'EXISTS',
              related: {
                correlation: {parentField: ['id'], childField: ['issue_id']},
                subquery: {table: 'issues', alias: 'zsubq_issue'},
              },
            },
          ],
        },
      ],
    },
  },
  {
    type: 'node-constraint',
    attemptNumber: 2,
    nodeType: 'connection',
    node: 'comments',
    branchPattern: [1, 0],
    constraint: {},
    from: 'x',
  },
  {
    type: 'node-cost',
    attemptNumber: 2,
    nodeType: 'join',
    node: 'j',
    branchPattern: [],
    downstreamChildSelectivity: 1,
    costEstimate: {
      startupCost: 0,
      scanEst: 0,
      cost: 0,
      returnedRows: 0,
      selectivity: 0,
      limit: 0,
    },
  },
  {
    type: 'node-cost',
    nodeType: 'filter',
    node: 'f',
    branchPattern: [],
    downstreamChildSelectivity: 1,
    costEstimate: {
      startupCost: 0,
      scanEst: 0,
      cost: 0,
      returnedRows: 0,
      selectivity: 0,
    },
  },
  {
    type: 'plan-complete',
    attemptNumber: 2,
    totalCost: 1.005,
    flipPattern: 1,
    joinStates: [],
  },
  {type: 'attempt-start', attemptNumber: 1, totalAttempts: 1},
  {
    type: 'best-plan-selected',
    bestAttemptNumber: 2,
    totalCost: 0.125,
    flipPattern: 1,
    joinStates: [],
  },
];

const analyzeResult = {
  warnings: ['a warning'],
  syncedRows: {issues: [issue('i1')]},
  syncedRowCount: 1,
  start: 1000,
  end: 1012.5,
  elapsed: 12.5,
  afterPermissions: "issues.where('owner_id', 'u1')",
  vendedRowCounts: {issues: {'SELECT 1': 3}},
  sqlitePlans: {'SELECT 1': ['SCAN issues']},
  readRowCount: 3,
  // The events' optional fields are undefined-able, which JSON values aren't.
  joinPlans: joinPlans as unknown as ReadonlyJSONValue,
};

export const inspectorScenarios: Scenario[] = [
  {
    name: 'inspector: serverVersion sends version and resolves with the answer',
    steps: [
      start,
      connected,
      {op: 'inspect', call: 'v', method: 'serverVersion'},
      answer(0, 'version', '0.26.1'),
    ],
  },
  {
    name: 'inspector: a request made before the connection is sent once connected',
    steps: [
      start,
      {op: 'inspect', call: 'v', method: 'serverVersion'},
      advance(10),
      connected,
      answer(0, 'version', '0.26.1'),
    ],
  },
  {
    name: 'inspector: an error answer rejects; answers to other IDs are ignored',
    steps: [
      start,
      connected,
      {op: 'inspect', call: 'a', method: 'serverVersion'},
      {op: 'inspect', call: 'b', method: 'serverVersion'},
      {
        op: 'receive',
        message: ['inspect', {op: 'version', id: 'nobody', value: 'x'}],
      },
      answer(1, 'error', 'Unknown inspector op'),
      answer(0, 'version', '1.0.0'),
    ],
  },
  {
    name: 'inspector: metrics merges the client metrics with the server digests; slow materializations warn',
    options: {slowMaterializeThreshold: 50, logLevel: 'debug'},
    steps: [
      start,
      connected,
      {op: 'materialize', view: 'all', query: 'issues.all'},
      advance(100),
      ...syncPoke('p1', null, '01', [allHash], issues(40)),
      {op: 'materialize', view: 'users', query: 'users.all'},
      advance(10),
      ...syncPoke(
        'p2',
        '01',
        '02',
        [usersHash],
        [put('users', user('u1', 'A'))],
      ),
      ...syncPoke('p3', '02', '03', [], issues(70).slice(30)),
      mutate('m1', 'issue.create', {id: 'j1'}),
      {op: 'inspect', call: 'metrics', method: 'metrics'},
      answer(0, 'metrics', serverMetrics),
    ],
  },
  {
    name: 'inspector: slowMaterializeThreshold at its default logs nothing at warn',
    steps: [
      start,
      connected,
      {op: 'materialize', view: 'all', query: 'issues.all'},
      advance(4990),
      ...syncPoke('p1', null, '01', [allHash], issues(1)),
      {op: 'materialize', view: 'users', query: 'users.all'},
      advance(5001),
      ...syncPoke('p2', '01', '02', [usersHash]),
    ],
  },
  {
    name: 'inspector: queries of the client and of its group, with the client and server metrics',
    steps: [
      start,
      connected,
      {op: 'materialize', view: 'users', query: 'users.all'},
      advance(10),
      ...syncPoke(
        'p1',
        null,
        '01',
        [usersHash],
        [put('users', user('u1', 'A'))],
      ),
      // Evicted before the inspector exists: forgotten.
      {op: 'destroy', view: 'users'},
      advance(10),
      {op: 'inspect', call: 'version', method: 'serverVersion'},
      answer(0, 'version', '1'),
      {op: 'materialize', view: 'all', query: 'issues.all'},
      advance(25),
      ...syncPoke('p2', '01', '02', [allHash], issues(40)),
      // Evicted once the inspector exists: kept for it.
      {op: 'destroy', view: 'all'},
      advance(10),
      {op: 'inspect', call: 'client', method: 'clientQueries'},
      answer(1, 'queries', queryRows),
      {op: 'inspect', call: 'group', method: 'clientGroupQueries'},
      answer(2, 'queries', [queryRows[2], queryRows[0]]),
    ],
  },
  {
    name: 'inspector: analyzeQuery, analyzeServerAST, and analyzeNamedQuery',
    steps: [
      start,
      connected,
      {
        op: 'inspect',
        call: 'named',
        method: 'analyzeQuery',
        query: 'issues.byOwner',
        args: 'u1',
        options: {vendedRows: true, joinPlans: true},
      },
      {
        op: 'inspect',
        call: 'ast',
        method: 'analyzeServerAST',
        ast: {
          table: 'issues',
          where: {
            type: 'simple',
            op: '=',
            left: {type: 'column', name: 'owner_id'},
            right: {type: 'literal', value: 'u1'},
          },
          orderBy: [['id', 'asc']],
        },
        options: {syncedRows: false},
      },
      {
        op: 'inspect',
        call: 'byName',
        method: 'analyzeNamedQuery',
        name: 'issues.byOwner',
        queryArgs: ['u2'],
      },
      answer(0, 'analyze-query', analyzeResult),
      answer(1, 'analyze-query', {
        warnings: [],
        syncedRowCount: 0,
        start: 1,
        end: 2,
      }),
      answer(2, 'error', 'No query named issues.byOwner'),
      {
        op: 'inspect',
        call: 'formatted',
        method: 'formatPlannerEvents',
        events: joinPlans,
      },
      {
        op: 'inspect',
        call: 'formattedMore',
        method: 'formatPlannerEvents',
        events: morePlans,
      },
      {op: 'inspect', call: 'formattedNone', method: 'formatPlannerEvents'},
    ],
  },
  {
    name: 'inspector: authenticate, then a refused request presents the accepted password again',
    steps: [
      start,
      connected,
      {op: 'inspect', call: 'auth', method: 'authenticate', password: 'secret'},
      authenticated(0, true),
      {op: 'inspect', call: 'v', method: 'serverVersion'},
      authenticated(1, false),
      authenticated(2, true),
      answer(3, 'version', '2.0.0'),
    ],
  },
  {
    name: 'inspector: unauthenticated without a password fails; a refused password resolves false',
    steps: [
      start,
      connected,
      {op: 'inspect', call: 'v1', method: 'serverVersion'},
      authenticated(0, false),
      {op: 'inspect', call: 'wrong', method: 'authenticate', password: 'nope'},
      authenticated(1, false),
      {op: 'inspect', call: 'v2', method: 'metrics'},
      authenticated(2, false),
      {op: 'inspect', call: 'right', method: 'authenticate', password: 'pw'},
      authenticated(3, true),
      {op: 'inspect', call: 'v3', method: 'metrics'},
      answer(4, 'metrics', serverMetrics),
    ],
  },
  {
    name: 'inspector: after a reconnect the accepted password is presented before retrying',
    steps: [
      start,
      connected,
      {op: 'inspect', call: 'auth', method: 'authenticate', password: 'secret'},
      authenticated(0, true),
      {op: 'socketClose', code: 1006},
      connected,
      {op: 'inspect', call: 'v', method: 'serverVersion'},
      authenticated(1, false),
      authenticated(2, true),
      answer(3, 'version', '3.0.0'),
      // The server refuses it now: forgotten, and the request fails.
      {op: 'socketClose', code: 1006},
      connected,
      {op: 'inspect', call: 'v2', method: 'serverVersion'},
      authenticated(4, false),
      authenticated(5, false),
      {op: 'inspect', call: 'v3', method: 'serverVersion'},
      authenticated(6, false),
    ],
  },
  {
    name: 'inspector: the clients, the map, and the rows of the local store',
    steps: [
      start,
      connected,
      {op: 'materialize', view: 'v', query: 'issues.byOwner', args: 'u1'},
      advance(10),
      ...syncPoke(
        'p1',
        null,
        '01',
        [byOwnerU1],
        [
          put('issues', issue('i1', {ownerId: 'u1'})),
          put('issues', issue('i2', {ownerId: 'u1'})),
        ],
      ),
      mutate('m1', 'issue.create', {id: 'i3', ownerId: 'u1'}),
      {op: 'inspect', call: 'clients', method: 'clients'},
      {op: 'inspect', call: 'groupClients', method: 'clientGroupClients'},
      {op: 'inspect', call: 'rows', method: 'clientRows', table: 'issue'},
      {op: 'inspect', call: 'none', method: 'clientRows', table: 'nope'},
      {op: 'inspect', call: 'map', method: 'clientMap'},
      {op: 'inspect', call: 'withQueries', method: 'clientsWithQueries'},
      answer(0, 'queries', [queryRows[0]]),
      {
        op: 'inspect',
        call: 'groupWithQueries',
        method: 'clientGroupClientsWithQueries',
      },
      answer(1, 'queries', []),
    ],
  },
  {
    name: 'inspector: clients after a restart on the same storage',
    steps: [
      start,
      connected,
      {op: 'restart'},
      {op: 'inspect', call: 'clients', method: 'clients'},
      {op: 'inspect', call: 'group', method: 'clientGroupClients'},
    ],
  },
];
