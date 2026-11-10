import {createServer, type Server} from 'node:http';
import {connect, type AddressInfo} from 'node:net';
import {afterEach, expect, test} from 'vitest';
import {createSilentLogContext} from '../../../shared/src/logging-test-utils.ts';
import type {Worker} from './processes.ts';
import {installWebSocketHandoff} from './websocket-handoff.ts';

let server: Server | undefined;
afterEach(() => {
  server?.close();
  server = undefined;
});

test('a client reset while a handoff is pending does not crash the process', async () => {
  const s = (server = createServer());
  let handoffStarted!: () => void;
  const started = new Promise<void>(r => (handoffStarted = r));
  // The handoff waits, as the runner's does for its worker.
  installWebSocketHandoff(createSilentLogContext(), () => handoffStarted(), s);
  await new Promise<void>(r => s.listen(0, '127.0.0.1', r));
  const {port} = s.address() as AddressInfo;

  const uncaught: unknown[] = [];
  const onUncaught = (e: unknown) => uncaught.push(e);
  process.on('uncaughtException', onUncaught);
  try {
    const client = connect(port, '127.0.0.1');
    await new Promise<void>(r => client.once('connect', r));
    client.write(
      'GET /sync HTTP/1.1\r\n' +
        'Host: 127.0.0.1\r\n' +
        'Upgrade: websocket\r\n' +
        'Connection: Upgrade\r\n' +
        'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n' +
        'Sec-WebSocket-Version: 13\r\n\r\n',
    );
    await started;
    client.resetAndDestroy();
    await new Promise(r => setTimeout(r, 200));
  } finally {
    process.off('uncaughtException', onUncaught);
  }
  expect(uncaught).toEqual([]);
});

test('a handoff whose socket closed on the way does not crash the process', () => {
  // Node delivers the message without the socket when the connection closed
  // while it was being sent to this process.
  let deliver!: (msg: unknown, socket?: unknown) => void;
  const parent = {
    onMessageType: (_type: string, handler: typeof deliver) => {
      deliver = handler;
    },
  } as unknown as Worker;
  installWebSocketHandoff(createSilentLogContext(), () => {}, parent);
  expect(() =>
    deliver({
      message: {url: '/sync/v57/connect', headers: {}},
      head: new ArrayBuffer(0),
      payload: undefined,
    }),
  ).not.toThrow();
});
