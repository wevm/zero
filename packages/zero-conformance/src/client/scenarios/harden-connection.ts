/**
 * Hardening scenarios for the connection and the sync protocol: hostile or
 * unexpected server messages, messages that arrive together with the
 * handshake, error bodies at the edges of their schemas, and timing.
 */
import type {Scenario} from '../harness.ts';
import {
  advance,
  connected,
  errorMessage,
  issue,
  mutate,
  poke,
  pokeMessages,
  pong,
  put,
  queryHash,
  start,
  syncPoke,
} from './helpers.ts';

const allHash = queryHash('issues.all');

export const hardenConnectionScenarios: Scenario[] = [
  {
    name: 'handshake: the initial poke right behind connected in one read',
    steps: [
      {op: 'materialize', view: 'v', query: 'issues.all'},
      start,
      {
        op: 'receiveAll',
        messages: [
          ['connected', {wsid: 'w1'}],
          ...pokeMessages('p1', null, '01', {
            gotQueriesPatch: [{op: 'put', hash: allHash}],
            rowsPatch: [put('issues', issue('i1'))],
          }),
        ],
      },
      advance(10),
    ],
  },
  {
    name: 'handshake: a close right after connected in the same turn',
    steps: [
      {op: 'materialize', view: 'v', query: 'issues.all'},
      start,
      connected,
      {op: 'socketClose', code: 1006},
      connected,
    ],
  },
  {
    name: 'ping: a pong that was never asked for, and one while connecting',
    steps: [start, pong, connected, pong, advance(5000), pong],
  },
  {
    name: 'ping: a poke while waiting for the pong does not end the wait',
    steps: [
      start,
      connected,
      advance(5000),
      ...poke('p1', null, '01', {}),
      advance(4999),
      advance(1),
    ],
  },
  {
    name: 'error: an unknown error kind is an invalid message',
    steps: [
      start,
      connected,
      errorMessage({kind: 'Teapot', message: 'short and stout'}),
      {op: 'connect'},
      connected,
    ],
  },
  {
    name: 'error: an error without a message is an invalid message',
    steps: [start, connected, errorMessage({kind: 'Internal'})],
  },
  {
    name: 'error: ServerOverloaded with minBackoffMs above maxBackoffMs',
    steps: [
      start,
      connected,
      errorMessage({
        kind: 'ServerOverloaded',
        message: 'busy',
        minBackoffMs: 9000,
        maxBackoffMs: 2000,
      }),
      advance(1999),
      advance(1),
      connected,
    ],
  },
  {
    name: 'error: ServerOverloaded with a zero maxBackoffMs reconnects at once',
    steps: [
      start,
      connected,
      errorMessage({
        kind: 'ServerOverloaded',
        message: 'busy',
        maxBackoffMs: 0,
      }),
      connected,
    ],
  },
  {
    name: 'error: reconnectParams keep their order and skip the standard ones',
    steps: [
      start,
      connected,
      errorMessage({
        kind: 'Rehome',
        message: 'go',
        maxBackoffMs: 10,
        reconnectParams: {
          'zone': 'b',
          'clientID': 'evil',
          '10': 'x',
          '9': 'y',
          'a': 'z',
        },
      }),
      advance(10),
    ],
  },
  {
    name: 'error: a backoff error while in backoff from another',
    steps: [
      start,
      connected,
      errorMessage({
        kind: 'Rebalance',
        message: 'a',
        minBackoffMs: 8000,
        reconnectParams: {first: '1'},
      }),
      advance(8000),
      errorMessage({
        kind: 'Rebalance',
        message: 'b',
        maxBackoffMs: 100,
      }),
      advance(100),
      connected,
    ],
  },
  {
    name: 'error: MutationRateLimited while connecting',
    steps: [
      start,
      mutate('m1', 'issue.create', {id: 'i1'}),
      errorMessage({kind: 'MutationRateLimited', message: 'slow'}),
      connected,
    ],
  },
  {
    name: 'error: ClientNotFound while connecting',
    steps: [
      start,
      errorMessage({
        kind: 'ClientNotFound',
        message: 'gone',
        origin: 'zeroCache',
      }),
      advance(10_000),
      {op: 'connect'},
      advance(10_000),
    ],
  },
  {
    name: 'error: InvalidConnectionRequestBaseCookie while connecting',
    steps: [
      start,
      errorMessage({
        kind: 'InvalidConnectionRequestBaseCookie',
        message: 'ahead',
        origin: 'zeroCache',
      }),
      {op: 'connect'},
      advance(10),
    ],
  },
  {
    name: 'error: SchemaVersionNotSupported while connecting, then connect()',
    steps: [
      start,
      mutate('m1', 'issue.create', {id: 'i1'}),
      errorMessage({
        kind: 'SchemaVersionNotSupported',
        message: 'old',
        origin: 'zeroCache',
      }),
      {op: 'connect'},
      connected,
      mutate('m2', 'issue.create', {id: 'i2'}),
    ],
  },
  {
    name: 'error: VersionNotSupported then connect()',
    steps: [
      start,
      connected,
      errorMessage({kind: 'VersionNotSupported', message: 'old'}),
      {op: 'connect'},
      connected,
    ],
  },
  {
    name: 'error: two errors in one read',
    steps: [
      start,
      connected,
      {
        op: 'receiveAll',
        messages: [
          ['error', {kind: 'Internal', message: 'first', origin: 'zeroCache'}],
          [
            'error',
            {kind: 'AuthInvalidated', message: 'second', origin: 'zeroCache'},
          ],
        ],
      },
    ],
  },
  {
    name: 'error: TransformFailed 401 from the API server, connect() with a token',
    steps: [
      {op: 'materialize', view: 'v', query: 'issues.all'},
      connected,
      errorMessage({
        kind: 'TransformFailed',
        origin: 'zeroCache',
        reason: 'http',
        status: 401,
        bodyPreview: '',
        message: 'no',
        queryIDs: [allHash],
      }),
      {op: 'connect', auth: 'token-2'},
      connected,
    ],
  },
  {
    name: 'auth: connect() with an empty token while needs-auth',
    options: {auth: 'token-1'},
    steps: [
      start,
      connected,
      errorMessage({
        kind: 'AuthInvalidated',
        message: 'x',
        origin: 'zeroCache',
      }),
      {op: 'connect', auth: ''},
      connected,
      {op: 'connect', auth: 'token-3'},
    ],
  },
  {
    name: 'auth: connect({auth}) right after the client is created',
    options: {auth: 'token-1'},
    steps: [{op: 'connect', auth: 'token-2'}, start, connected],
  },
  {
    name: 'auth: connect() twice while needs-auth',
    steps: [
      start,
      connected,
      errorMessage({
        kind: 'AuthInvalidated',
        message: 'x',
        origin: 'zeroCache',
      }),
      {op: 'connect', auth: 'a'},
      {op: 'connect', auth: 'b'},
      connected,
    ],
  },
  {
    name: 'auth: connect() in needs-auth while hidden',
    options: {hiddenTabDisconnectDelay: 0},
    steps: [
      start,
      connected,
      errorMessage({
        kind: 'AuthInvalidated',
        message: 'x',
        origin: 'zeroCache',
      }),
      {op: 'visibility', state: 'hidden'},
      advance(10),
      {op: 'connect', auth: 'a'},
      advance(10_000),
      {op: 'visibility', state: 'visible'},
      advance(10),
      connected,
    ],
  },
  {
    name: 'visibility: hidden and visible again within one turn',
    options: {hiddenTabDisconnectDelay: 0},
    steps: [
      start,
      connected,
      {op: 'visibility', state: 'hidden'},
      {op: 'visibility', state: 'visible'},
      advance(10),
      {op: 'visibility', state: 'hidden'},
      advance(1),
      {op: 'visibility', state: 'visible'},
      advance(1),
      connected,
    ],
  },
  {
    name: 'visibility: hidden during the backoff',
    options: {hiddenTabDisconnectDelay: 0},
    steps: [
      start,
      connected,
      {op: 'socketClose', code: 1006},
      {op: 'socketClose', code: 1006},
      {op: 'visibility', state: 'hidden'},
      advance(5000),
      advance(60_000),
      {op: 'visibility', state: 'visible'},
      connected,
    ],
  },
  {
    name: 'visibility: hidden in the error state, then connect() and visible',
    options: {hiddenTabDisconnectDelay: 0},
    steps: [
      start,
      connected,
      errorMessage({kind: 'Internal', message: 'x', origin: 'zeroCache'}),
      {op: 'visibility', state: 'hidden'},
      advance(1),
      {op: 'connect'},
      advance(1),
      {op: 'visibility', state: 'visible'},
      connected,
    ],
  },
  {
    name: 'timeout: connect timeout, then the late connected on the old socket',
    steps: [
      advance(10_000),
      {op: 'receive', message: ['connected', {wsid: 'w1'}], socket: 0},
      advance(5000),
      connected,
    ],
  },
  {
    name: 'disconnect timeout: offline, then an error on the next attempt',
    options: {disconnectTimeoutMs: 5000},
    steps: [
      advance(10_000),
      advance(5000),
      errorMessage({kind: 'Internal', message: 'x', origin: 'zeroCache'}),
      {op: 'connect'},
      connected,
    ],
  },
  {
    name: 'disconnect timeout: offline, then a Rebalance on the next attempt',
    options: {disconnectTimeoutMs: 5000},
    steps: [
      advance(10_000),
      advance(5000),
      errorMessage({kind: 'Rebalance', message: 'x', minBackoffMs: 7000}),
      advance(6999),
      advance(1),
      connected,
    ],
  },
  {
    name: 'close(): messages after close are ignored',
    steps: [
      {op: 'materialize', view: 'v', query: 'issues.all'},
      connected,
      {op: 'close'},
      {op: 'receive', socket: 0, message: ['pong', {}]},
      ...syncPoke(
        'p1',
        null,
        '01',
        [allHash],
        [put('issues', issue('i1'))],
      ).map(s => ({...s, socket: 0})),
      {op: 'socketClose', code: 1006, socket: 0},
      advance(10_000),
    ],
  },
  {
    name: 'close(): during the backoff',
    steps: [
      start,
      {op: 'socketClose', code: 1006},
      {op: 'close'},
      advance(10_000),
    ],
  },
  {
    name: 'close(): in needs-auth, then connect()',
    steps: [
      start,
      connected,
      errorMessage({
        kind: 'AuthInvalidated',
        message: 'x',
        origin: 'zeroCache',
      }),
      {op: 'close'},
      {op: 'connect', auth: 'b'},
      advance(10_000),
    ],
  },
  {
    name: 'poke: a canceled pokeEnd for another poke is an error',
    steps: [
      start,
      connected,
      {op: 'receive', message: ['pokeStart', {pokeID: 'p1', baseCookie: null}]},
      {
        op: 'receive',
        message: ['pokeEnd', {pokeID: 'p2', cookie: '01', cancel: true}],
      },
    ],
  },
  {
    name: 'poke: a canceled pokeEnd without a poke is an error',
    steps: [
      start,
      connected,
      {
        op: 'receive',
        message: ['pokeEnd', {pokeID: 'p2', cookie: '01', cancel: true}],
      },
    ],
  },
  {
    name: 'poke: rows missing primary key columns',
    steps: [
      {op: 'materialize', view: 'v', query: 'issues.all'},
      connected,
      ...syncPoke(
        'p1',
        null,
        '01',
        [allHash],
        [{op: 'put', tableName: 'issues', value: {title: 'no id'}}],
      ),
      advance(10),
    ],
  },
  {
    name: 'poke: update of a row the client does not have',
    steps: [
      {op: 'materialize', view: 'v', query: 'issues.all'},
      connected,
      ...syncPoke(
        'p1',
        null,
        '01',
        [allHash],
        [
          {
            op: 'update',
            tableName: 'issues',
            id: {id: 'nope'},
            merge: {title: 'T'},
          },
        ],
      ),
      advance(10),
    ],
  },
  {
    name: 'poke: a row with a numeric primary key in a string column',
    steps: [
      {op: 'materialize', view: 'v', query: 'issues.all'},
      connected,
      ...syncPoke(
        'p1',
        null,
        '01',
        [allHash],
        [
          {op: 'put', tableName: 'issues', value: {...issue('i1'), id: 1}},
          put('issues', issue('1')),
        ],
      ),
      advance(10),
    ],
  },
  {
    name: 'poke: desiredQueriesPatches are applied in the order the server sent them',
    steps: [
      {op: 'materialize', view: 'v', query: 'issues.all'},
      connected,
      ...poke('p1', null, '01', {
        desiredQueriesPatches: {
          '~other': [{op: 'clear'}],
          '$clientID': [{op: 'put', hash: allHash}],
        },
      }),
      {op: 'socketClose', code: 1006},
      advance(10),
    ],
  },
  {
    name: 'poke: an empty pokeID and an empty cookie',
    steps: [
      {op: 'materialize', view: 'v', query: 'issues.all'},
      connected,
      ...syncPoke('', null, '', [allHash], [put('issues', issue('i1'))]),
      ...syncPoke('', '', '01', [], [put('issues', issue('i2'))]),
      advance(10),
    ],
  },
  {
    name: 'poke: a poke while connecting, before connected',
    steps: [
      {op: 'materialize', view: 'v', query: 'issues.all'},
      start,
      ...syncPoke('p1', null, '01', [allHash], [put('issues', issue('i1'))]),
      connected,
      advance(10),
    ],
  },
  {
    name: 'poke: a poke that spans a reconnect is dropped',
    steps: [
      {op: 'materialize', view: 'v', query: 'issues.all'},
      connected,
      {op: 'receive', message: ['pokeStart', {pokeID: 'p1', baseCookie: null}]},
      {op: 'socketClose', code: 1006},
      connected,
      {
        op: 'receive',
        message: [
          'pokePart',
          {pokeID: 'p1', rowsPatch: [put('issues', issue('i1'))]},
        ],
      },
      advance(10),
    ],
  },
  {
    name: 'poke: an applied poke whose frame is still waiting when the socket drops',
    steps: [
      {op: 'materialize', view: 'v', query: 'issues.all'},
      connected,
      {
        op: 'receiveAll',
        messages: [
          ...pokeMessages('p1', null, '01', {
            gotQueriesPatch: [{op: 'put', hash: allHash}],
            rowsPatch: [put('issues', issue('i1'))],
          }),
        ],
      },
      {op: 'socketClose', code: 1006},
      connected,
    ],
  },
  {
    name: 'message: a pushResponse for another client is ignored',
    steps: [
      start,
      connected,
      mutate('m1', 'issue.create', {id: 'i1'}),
      {
        op: 'receive',
        message: [
          'pushResponse',
          {mutations: [{id: {clientID: 'other', id: 1}, result: {}}]},
        ],
      },
      advance(10),
    ],
  },
  {
    name: 'message: a pushResponse error for a mutation not yet sent',
    steps: [
      start,
      connected,
      {
        op: 'receive',
        message: [
          'pushResponse',
          {
            error: 'unsupportedPushVersion',
            mutationIDs: [{clientID: '$clientID', id: 7}],
          },
        ],
      },
      mutate('m1', 'issue.create', {id: 'i1'}),
    ],
  },
  {
    name: 'message: a pull response with no pending pull',
    steps: [
      start,
      connected,
      advance(4000),
      {
        op: 'receive',
        message: [
          'pull',
          {cookie: '05', requestID: 'r', lastMutationIDChanges: {}},
        ],
      },
      advance(4000),
    ],
  },
  {
    name: 'message: a transformError for a query nobody asked for',
    steps: [
      {op: 'materialize', view: 'v', query: 'issues.all'},
      connected,
      {
        op: 'receive',
        message: [
          'transformError',
          [{error: 'app', id: 'zzz', name: 'nope', message: 'x'}],
        ],
      },
      ...syncPoke('p1', null, '01', [allHash], [put('issues', issue('i1'))]),
      advance(10),
    ],
  },
  {
    name: 'message: deleteClients from the server',
    steps: [
      start,
      connected,
      {op: 'receive', message: ['deleteClients', {clientIDs: ['x']}]},
      {op: 'socketClose', code: 1006},
      connected,
    ],
  },
  {
    name: 'message: a binary frame of an unknown type',
    steps: [start, connected, {op: 'receiveBinary', bytes: [7, 1, 2]}],
  },
  {
    name: 'message: an empty binary frame',
    steps: [start, connected, {op: 'receiveBinary', bytes: []}],
  },
  {
    name: 'message: a tuple with an extra element',
    steps: [start, connected, {op: 'receive', message: ['pong', {}, 1]}],
  },
  {
    name: 'socket: a close code 1009 while connecting',
    steps: [
      start,
      {op: 'socketClose', code: 1009},
      advance(10_000),
      {op: 'connect'},
      advance(10_000),
    ],
  },
  {
    name: 'socket: a close reported twice',
    steps: [
      start,
      connected,
      {op: 'socketClose', code: 1006},
      {op: 'socketClose', code: 1006, socket: 0},
      connected,
    ],
  },
  {
    name: 'socket: a close code 1009 after an error',
    steps: [
      start,
      connected,
      errorMessage({kind: 'Internal', message: 'x', origin: 'zeroCache'}),
      {op: 'socketClose', code: 1009, socket: 0},
    ],
  },
  {
    name: 'header: an auth token over maxHeaderLength is still sent',
    options: {maxHeaderLength: 10, auth: 'a-long-token-for-a-tiny-header'},
    steps: [start, connected],
  },
  {
    name: 'reentrancy: connect({auth}) from a needs-auth listener',
    steps: [
      start,
      connected,
      {
        op: 'onState',
        state: 'needs-auth',
        then: {op: 'connect', auth: 'token-2'},
      },
      errorMessage({
        kind: 'AuthInvalidated',
        message: 'x',
        origin: 'zeroCache',
      }),
      connected,
    ],
  },
  {
    name: 'reentrancy: connect() from an error listener',
    steps: [
      start,
      connected,
      {op: 'onState', state: 'error', then: {op: 'connect'}},
      errorMessage({kind: 'Internal', message: 'x', origin: 'zeroCache'}),
      connected,
    ],
  },
  {
    name: 'reentrancy: close() from a connected listener',
    steps: [
      {op: 'materialize', view: 'v', query: 'issues.all'},
      mutate('m1', 'issue.create', {id: 'i1'}),
      start,
      {op: 'onState', state: 'connected', then: {op: 'close'}},
      connected,
      advance(10_000),
    ],
  },
  {
    name: 'reentrancy: close() from a connecting listener after a drop',
    steps: [
      start,
      connected,
      {op: 'onState', state: 'connecting', then: {op: 'close'}},
      {op: 'socketClose', code: 1006},
      advance(10_000),
    ],
  },
  {
    name: 'reentrancy: close() from a disconnected listener',
    options: {disconnectTimeoutMs: 5000},
    steps: [
      start,
      {op: 'onState', state: 'disconnected', then: {op: 'close'}},
      advance(10_000),
      advance(10_000),
    ],
  },
  {
    name: 'reentrancy: close() from a needs-auth listener',
    steps: [
      start,
      connected,
      mutate('m1', 'issue.create', {id: 'i1'}),
      {op: 'onState', state: 'needs-auth', then: {op: 'close'}},
      errorMessage({kind: 'Unauthorized', message: 'x', origin: 'zeroCache'}),
      advance(10_000),
    ],
  },
];
