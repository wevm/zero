/**
 * Hardening scenarios for mutations: CRUD edge cases inside a mutator,
 * mutators that throw midway or only on a rebase, what a mutator returns,
 * and push responses and stored results the server sends in odd orders and
 * shapes.
 */
import type {ReadonlyJSONValue} from '../../../../shared/src/json.ts';
import type {Scenario, Step} from '../harness.ts';
import {
  advance,
  connected,
  gotPut,
  issue,
  mutate,
  poke,
  pushResponse,
  put,
  queryHash,
  start,
  syncPoke,
} from './helpers.ts';

type JSONObject = {readonly [key: string]: ReadonlyJSONValue};

const allHash = queryHash('issues.all');
const labelsHash = queryHash('issueLabels.all');

const viewAll = {op: 'materialize', view: 'v', query: 'issues.all'} as const;

type Op = ['insert' | 'upsert' | 'update' | 'delete', string, JSONObject];

/** A `harden.ops` mutation. */
function ops(mutation: string, list: Op[], more: JSONObject = {}): Step {
  return mutate(mutation, 'harden.ops', {
    ops: list.map(([op, table, value]) => ({op, table, value})),
    ...more,
  });
}

/** A full `issue` row with client column names. */
function row(id: string, fields: JSONObject = {}): JSONObject {
  return {
    id,
    title: id,
    description: '',
    closed: false,
    ownerId: null,
    createdAt: 1,
    ...fields,
  };
}

function mutationResult(
  id: number,
  result: JSONObject,
  clientID = '$clientID',
): ReadonlyJSONValue {
  return {op: 'put', mutation: {id: {clientID, id}, result}};
}

export const hardenMutationScenarios: Scenario[] = [
  {
    name: 'harden crud: insert null-fills omitted columns and the transaction reads its own writes',
    steps: [
      viewAll,
      connected,
      ops('m1', [['insert', 'issue', {id: 'i1', title: 'only a title'}]], {
        read: ['issue'],
      }),
      ops('m2', [['insert', 'issue', {id: 'i2', title: 't', ownerId: 'u9'}]], {
        read: ['issue'],
      }),
    ],
  },
  {
    name: 'harden crud: a column the schema does not have',
    steps: [
      viewAll,
      connected,
      ops('m1', [['insert', 'issue', row('i1', {extra: 'x'})]], {
        read: ['issue'],
      }),
      ops('m2', [['update', 'issue', {id: 'i1', other: 5, title: 'u'}]], {
        read: ['issue'],
      }),
      ops('m3', [['upsert', 'issue', row('i2', {more: true})]], {
        read: ['issue'],
      }),
    ],
  },
  {
    name: 'harden crud: every operation on one row in one transaction',
    steps: [
      viewAll,
      connected,
      ops(
        'm1',
        [
          ['insert', 'issue', row('i1', {title: 'first'})],
          ['insert', 'issue', row('i1', {title: 'dup'})],
          ['upsert', 'issue', {id: 'i1', title: 'merged'}],
          ['update', 'issue', {id: 'i9', title: 'missing'}],
          ['delete', 'issue', {id: 'i9'}],
          ['update', 'issue', {id: 'i1', ownerId: 'u1', closed: true}],
        ],
        {read: ['issue']},
      ),
      ops(
        'm2',
        [
          ['delete', 'issue', {id: 'i1', title: 'ignored'}],
          ['update', 'issue', {id: 'i1', title: 'after delete'}],
          ['insert', 'issue', {id: 'i1', title: 'again'}],
          ['update', 'issue', {id: 'i1', title: null, ownerId: null}],
        ],
        {read: ['issue']},
      ),
      // Changes nothing: the main branch sees no difference.
      ops('m3', [['update', 'issue', {id: 'i1'}]]),
      ops('m4', [['upsert', 'issue', {id: 'i1', title: null}]]),
    ],
  },
  {
    name: 'harden crud: a compound primary key',
    steps: [
      {op: 'materialize', view: 'l', query: 'issueLabels.all'},
      connected,
      ...syncPoke(
        'p1',
        null,
        '01',
        [labelsHash],
        [put('issueLabel', {issueId: 'a', labelId: '2'})],
      ),
      ops(
        'm1',
        [
          ['insert', 'issueLabel', {issueId: 'a', labelId: '1'}],
          ['insert', 'issueLabel', {issueId: 'a', labelId: '2', extra: 1}],
          ['upsert', 'issueLabel', {issueId: 'b', labelId: '1'}],
          ['delete', 'issueLabel', {issueId: 'a', labelId: '2'}],
        ],
        {read: ['issueLabel']},
      ),
      ...poke('p2', '01', '02', {
        lastMutationIDChanges: {$clientID: 1},
        rowsPatch: [
          {
            op: 'del',
            tableName: 'issueLabel',
            id: {issueId: 'a', labelId: '2'},
          },
          put('issueLabel', {issueId: 'a', labelId: '1'}),
          put('issueLabel', {issueId: 'b', labelId: '1'}),
        ],
      }),
    ],
  },
  {
    name: 'harden crud: a missing or null primary key',
    steps: [
      viewAll,
      connected,
      ops('m1', [['insert', 'issue', {title: 'no id'}]]),
      ops('m2', [['insert', 'issue', {id: null, title: 'null id'}]]),
      ops('m3', [['delete', 'issue', {}]]),
      ops('m4', [['update', 'issueLabel', {issueId: 'a'}]]),
      ops('m5', [['insert', 'issue', {id: {nested: 1}}]]),
      ops('m6', [['insert', 'issue', row('ok')]], {read: ['issue']}),
    ],
  },
  {
    name: 'harden crud: a number primary key and a string one',
    steps: [
      viewAll,
      connected,
      ops('m1', [['insert', 'issue', row('5')]]),
      ops('m2', [['insert', 'issue', row('x', {id: 5, title: 'number'})]]),
      ops('m3', [['upsert', 'issue', {id: 5, title: 'upsert by number'}]], {
        read: ['issue'],
      }),
      ops('m4', [['delete', 'issue', {id: 5}]], {read: ['issue']}),
    ],
  },
  {
    name: 'harden crud: a number primary key among string ones',
    steps: [
      viewAll,
      connected,
      ops('m1', [['insert', 'issue', row('a')]]),
      ops('m2', [['insert', 'issue', row('x', {id: 7})]]),
      ops('m3', [['insert', 'issue', row('b')]], {read: ['issue']}),
    ],
  },
  {
    name: 'harden crud: a value of the wrong type in a sorted column',
    steps: [
      {op: 'materialize', view: 'open', query: 'issues.open'},
      connected,
      ops('m1', [['insert', 'issue', row('a', {createdAt: 2})]]),
      ops('m2', [['insert', 'issue', row('b', {createdAt: 'soon'})]]),
      ops('m3', [['update', 'issue', {id: 'a', createdAt: true}]]),
      ops('m4', [['insert', 'issue', row('c', {closed: 'no'})]], {
        read: ['issue'],
      }),
    ],
  },
  {
    name: 'harden throw: a mutator that throws after updating and deleting leaves the rows alone',
    steps: [
      viewAll,
      connected,
      ...syncPoke(
        'p1',
        null,
        '01',
        [allHash],
        [put('issues', issue('i1')), put('issues', issue('i2'))],
      ),
      ops(
        'm1',
        [
          ['update', 'issue', {id: 'i1', closed: true, title: 'changed'}],
          ['delete', 'issue', {id: 'i2'}],
          ['insert', 'issue', row('i3')],
          ['upsert', 'issue', {id: 'i1', title: 'changed again'}],
        ],
        {fail: 'midway', details: {step: 4}},
      ),
      ops('m2', [], {read: ['issue']}),
      advance(10),
    ],
  },
  {
    name: 'harden throw: details of an app error the mutator throws',
    steps: [
      start,
      connected,
      ops('m1', [], {fail: 'no details'}),
      ops('m2', [], {fail: 'null details', details: null}),
      ops('m3', [], {fail: 'zero details', details: 0}),
      ops('m4', [], {fail: '', details: [1, 'two']}),
    ],
  },
  {
    name: 'harden rebase: a mutator that throws only on rebase drops its writes for the next mutation',
    steps: [
      viewAll,
      connected,
      ...syncPoke('p1', null, '01', [allHash], [put('issues', issue('i1'))]),
      ops('m1', [['insert', 'issue', row('mine')]], {failOnRebase: true}),
      ops('m2', [['update', 'issue', {id: 'mine', title: 'edited'}]]),
      ops('m3', [], {read: ['issue']}),
      ...syncPoke('p2', '01', '02', [], [put('issues', issue('i2'))]),
      ops('m4', [], {read: ['issue']}),
      ...poke('p3', '02', '03', {lastMutationIDChanges: {$clientID: 2}}),
      ...poke('p4', '03', '04', {
        lastMutationIDChanges: {$clientID: 4},
        rowsPatch: [put('issues', issue('mine', {title: 'server'}))],
      }),
    ],
  },
  {
    name: 'harden restart: a pending mutation that throws on rebase, after a restart',
    steps: [
      viewAll,
      start,
      connected,
      ...syncPoke('p1', null, '01', [allHash], [put('issues', issue('i1'))]),
      ops('m1', [['insert', 'issue', row('mine')]], {failOnRebase: true}),
      advance(2000),
      {op: 'restart'},
      viewAll,
      start,
      ops('m2', [], {read: ['issue']}),
      connected,
      ...syncPoke('p2', '01', '02', [], [put('issues', issue('i2'))]),
      ...poke('p3', '02', '03', {
        lastMutationIDChanges: {'$clientID.0': 1, '$clientID': 1},
      }),
    ],
  },
  {
    name: 'harden rebase: pending insert, delete and update over server changes to the same rows',
    steps: [
      viewAll,
      connected,
      ...syncPoke(
        'p1',
        null,
        '01',
        [allHash],
        [put('issues', issue('b')), put('issues', issue('c'))],
      ),
      ops('m1', [['insert', 'issue', row('a', {title: 'mine'})]]),
      ops('m2', [['delete', 'issue', {id: 'b'}]]),
      ops('m3', [['update', 'issue', {id: 'c', title: 'edited'}]]),
      ops('m4', [['upsert', 'issue', {id: 'd', title: 'upserted'}]]),
      ...poke('p2', '01', '02', {
        rowsPatch: [
          put('issues', issue('a', {title: 'server a'})),
          {
            op: 'update',
            tableName: 'issues',
            id: {id: 'b'},
            merge: {title: 'server b'},
          },
          {op: 'del', tableName: 'issues', id: {id: 'c'}},
          put('issues', issue('d', {title: 'server d', closed: true})),
        ],
      }),
      ops('m5', [], {read: ['issue']}),
      ...poke('p3', '02', '03', {lastMutationIDChanges: {$clientID: 4}}),
    ],
  },
  {
    name: 'harden reconnect: mutations the client gave up on are pushed again and late results ignored',
    options: {disconnectTimeoutMs: 10_000},
    steps: [
      viewAll,
      connected,
      ops('m1', [['insert', 'issue', row('a')]]),
      ops('m2', [['insert', 'issue', row('b')]]),
      {op: 'socketClose', code: 1006},
      advance(5000),
      advance(5000),
      advance(1000),
      ops('m3', [['insert', 'issue', row('c')]]),
      advance(5000),
      connected,
      pushResponse([
        {id: 1, result: {error: 'app', message: 'late'}},
        {id: 2, result: {data: 'late'}},
      ]),
      ops('m4', [['insert', 'issue', row('d')]]),
      ...poke('p1', null, '01', {
        lastMutationIDChanges: {$clientID: 3},
        gotQueriesPatch: [gotPut(allHash)],
        rowsPatch: [put('issues', issue('a')), put('issues', issue('b'))],
      }),
    ],
  },
  {
    name: 'harden result: what a mutator returns',
    steps: [
      start,
      connected,
      ops('m1', []),
      ops('m2', [], {return: null}),
      ops('m3', [], {return: 0}),
      ops('m4', [], {return: false}),
      ops('m5', [], {return: ''}),
      ops('m6', [], {
        return: {'a': [1, {b: null}], '': 'empty key', '1': 'one'},
      }),
      ops('m7', [], {return: -0}),
      ops('m8', [], {return: 1e21}),
      pushResponse([
        {id: 1, result: {}},
        {id: 2, result: {data: null}},
        {id: 3, result: {data: 0}},
        {id: 4, result: {data: false}},
        {id: 5, result: {data: ''}},
        {id: 6, result: {data: {x: 1}, extra: 'ignored'}},
        {id: 7, result: {data: -0}},
        {id: 8, result: {data: 5e-7}},
      ]),
    ],
  },
  {
    name: 'harden result: mutation args with unusual numbers and keys are pushed as JS writes them',
    steps: [
      start,
      connected,
      mutate('m1', 'harden.ops', {
        'ops': [],
        'return': 1,
        'n': [-0, 1e21, 1e-7, 0.1, 123456789012345680000, 2 ** 53 + 2],
        '10': 'ten',
        '2': 'two',
        'b': 'b',
        'a': 'a',
        '\u2028': 'ls',
        '\ud83d\ude00': 'emoji',
      }),
    ],
  },
  {
    name: 'harden push: results out of order, twice, and for unknown mutations',
    steps: [
      start,
      connected,
      ops('m1', []),
      ops('m2', []),
      ops('m3', []),
      pushResponse([
        {id: 3, result: {data: 'three'}},
        {id: 1, result: {data: 'one'}},
        {id: 1, result: {error: 'app', message: 'second answer for 1'}},
        {id: 99, result: {error: 'app', message: 'unknown'}},
      ]),
      pushResponse([{id: 2, result: {error: 'alreadyProcessed'}}]),
      pushResponse([{id: 2, result: {data: 'two'}}]),
      ...poke('p1', null, '01', {lastMutationIDChanges: {$clientID: 3}}),
    ],
  },
  {
    name: 'harden push: app errors whose details are falsy',
    steps: [
      start,
      connected,
      ops('m1', []),
      ops('m2', []),
      ops('m3', []),
      ops('m4', []),
      ops('m5', []),
      pushResponse([
        {id: 1, result: {error: 'app', message: 'zero', details: 0}},
        {id: 2, result: {error: 'app', message: 'empty', details: ''}},
        {id: 3, result: {error: 'app', message: 'false', details: false}},
        {id: 4, result: {error: 'app', message: 'null', details: null}},
        {id: 5, result: {error: 'app', details: {kept: true}}},
      ]),
    ],
  },
  {
    name: 'harden push: stored app errors whose details are falsy',
    steps: [
      start,
      connected,
      ops('m1', []),
      ops('m2', []),
      ...poke('p1', null, '01', {
        lastMutationIDChanges: {$clientID: 2},
        mutationsPatch: [
          mutationResult(1, {error: 'app', message: 'zero', details: 0}),
          mutationResult(2, {error: 'app', message: 'false', details: false}),
        ],
      }),
    ],
  },
  {
    name: 'harden push: a pushResponse for another client is an error',
    steps: [
      start,
      connected,
      ops('m1', []),
      {
        op: 'receive',
        message: [
          'pushResponse',
          {mutations: [{id: {clientID: 'other', id: 1}, result: {}}]},
        ],
      },
      advance(5000),
    ],
  },
  {
    name: 'harden push: an app error pushResponse for another client is an error',
    steps: [
      start,
      connected,
      ops('m1', []),
      {
        op: 'receive',
        message: [
          'pushResponse',
          {
            mutations: [
              {
                id: {clientID: 'other', id: 1},
                result: {error: 'app', message: 'x'},
              },
            ],
          },
        ],
      },
    ],
  },
  {
    name: 'harden push: an error kind the client does not know',
    steps: [
      start,
      connected,
      ops('m1', []),
      pushResponse([{id: 1, result: {error: 'brandNew', message: 'x'}}]),
    ],
  },
  {
    name: 'harden push: a pushResponse on a socket the client closed is ignored',
    steps: [
      start,
      connected,
      ops('m1', []),
      ops('m2', []),
      {op: 'socketClose', code: 1006},
      advance(5000),
      connected,
      pushResponse([{id: 1, result: {data: 'late'}}]),
      {
        op: 'receive',
        socket: 0,
        message: [
          'pushResponse',
          {mutations: [{id: {clientID: '$clientID', id: 2}, result: {}}]},
        ],
      },
    ],
  },
  {
    name: 'harden poke: stored oooMutation and alreadyProcessed results',
    steps: [
      start,
      connected,
      ops('m1', []),
      ops('m2', []),
      ...poke('p1', null, '01', {
        mutationsPatch: [mutationResult(1, {error: 'alreadyProcessed'})],
      }),
      ...poke('p2', '01', '02', {
        mutationsPatch: [
          mutationResult(2, {error: 'oooMutation', details: 'gap'}),
        ],
      }),
    ],
  },
  {
    name: 'harden poke: stored results for another client and deletes of unknown ones',
    steps: [
      start,
      connected,
      ops('m1', []),
      ...poke('p1', null, '01', {
        mutationsPatch: [
          mutationResult(1, {data: 'not mine'}, 'other'),
          {op: 'del', id: {clientID: '$clientID', id: 7}},
        ],
      }),
      ...poke('p2', '01', '02', {
        mutationsPatch: [mutationResult(1, {data: 'mine'})],
      }),
      pushResponse([{id: 1, result: {data: 'late push'}}]),
    ],
  },
  {
    name: 'harden poke: a stored result before its lmid and a pushResponse after',
    steps: [
      viewAll,
      connected,
      ops('m1', [['insert', 'issue', row('i1')]]),
      ops('m2', [['insert', 'issue', row('i2')]]),
      ...poke('p1', null, '01', {
        gotQueriesPatch: [gotPut(allHash)],
        mutationsPatch: [mutationResult(2, {data: 'stored'})],
      }),
      ...poke('p2', '01', '02', {
        lastMutationIDChanges: {$clientID: 2},
        rowsPatch: [put('issues', issue('i1')), put('issues', issue('i2'))],
      }),
      pushResponse([
        {id: 1, result: {error: 'app', message: 'too late'}},
        {id: 2, result: {error: 'app', message: 'too late'}},
      ]),
    ],
  },
  {
    name: 'harden lmid: a lastMutationID past every pending mutation',
    steps: [
      viewAll,
      connected,
      ops('m1', [['insert', 'issue', row('i1')]]),
      ops('m2', [['insert', 'issue', row('i2')]]),
      ...poke('p1', null, '01', {
        lastMutationIDChanges: {$clientID: 5},
        gotQueriesPatch: [gotPut(allHash)],
      }),
      ops('m3', [['insert', 'issue', row('i3')]], {read: ['issue']}),
      ...poke('p2', '01', '02', {lastMutationIDChanges: {$clientID: 6}}),
    ],
  },
  {
    name: 'harden lmid: many pending mutations replay in order across reconnects',
    steps: [
      viewAll,
      connected,
      ...syncPoke('p1', null, '01', [allHash]),
      ops('m1', [['insert', 'issue', row('a')]]),
      ops('m2', [['update', 'issue', {id: 'a', title: 'a2'}]]),
      ops('m3', [['insert', 'issue', row('b')]]),
      ops('m4', [['delete', 'issue', {id: 'a'}]]),
      ops('m5', [['upsert', 'issue', {id: 'b', closed: true}]]),
      {op: 'socketClose', code: 1006},
      advance(5000),
      connected,
      ...poke('p2', '01', '02', {
        lastMutationIDChanges: {$clientID: 2},
        rowsPatch: [put('issues', issue('a', {title: 'a2'}))],
      }),
      {op: 'socketClose', code: 1006},
      advance(5000),
      connected,
      ops('m6', [['insert', 'issue', row('c')]]),
      ...poke('p3', '02', '03', {
        lastMutationIDChanges: {$clientID: 6},
        rowsPatch: [
          {op: 'del', tableName: 'issues', id: {id: 'a'}},
          put('issues', issue('b', {closed: true})),
          put('issues', issue('c')),
        ],
      }),
    ],
  },
];
