/**
 * Tests for server.ts's Postgres checks, at startup and while it runs,
 * against real `psql`:
 *
 *   node --test apps/zbugs-swift/server/postgres.test.ts
 */
import assert from 'node:assert/strict';
import {existsSync, mkdtempSync, rmSync} from 'node:fs';
import {createServer, type Server, type Socket} from 'node:net';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {test, type TestContext} from 'node:test';
import {waitForPostgres, watchPostgres} from './postgres.ts';

// psql's shortest connect timeout, so that a check that times out is quick.
const env = {...process.env, ['PGCONNECT_TIMEOUT']: '2'};
const sleep = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));

function listen(server: Server): Promise<number> {
  return new Promise(resolve =>
    server.listen(0, '127.0.0.1', () => {
      resolve((server.address() as {port: number}).port);
    }),
  );
}

/** A file that the watcher's start command creates. */
function startedFile(t: TestContext) {
  const dir = mkdtempSync(join(tmpdir(), 'zbugs-pg-start-'));
  t.after(() => rmSync(dir, {recursive: true, force: true}));
  const file = join(dir, 'started');
  return {file, start: `touch '${file}'`};
}

/** A Postgres message: its type, its length, and `body`. */
function message(type: string, body: Buffer) {
  const length = Buffer.alloc(4);
  length.writeInt32BE(body.length + 4);
  return Buffer.concat([Buffer.from(type), length, body]);
}

/**
 * Answers psql as Postgres does with `sslmode=disable`: lets it in without a
 * password, then answers each query as one that returns no rows.
 */
function answer(socket: Socket) {
  const readyForQuery = message('Z', Buffer.from('I'));
  socket.once('data', () => {
    socket.write(Buffer.concat([message('R', Buffer.alloc(4)), readyForQuery]));
    socket.on('data', data => {
      if (data.toString('latin1', 0, 1) === 'Q') {
        socket.write(
          Buffer.concat([
            message('C', Buffer.from('SELECT 0\0')),
            readyForQuery,
          ]),
        );
      }
    });
  });
}

/** Waits for Postgres at startup, as server.ts does, until the test ends. */
function startup(t: TestContext, url: string, start: string, interval: number) {
  const aborting = new AbortController();
  t.after(() => aborting.abort());
  const wait = waitForPostgres(url, env, start, interval, aborting.signal);
  const result = {settled: false, wait};
  void wait.then(
    () => (result.settled = true),
    () => (result.settled = true),
  );
  return result;
}

for (const [behavior, unanswered] of [
  // psql times out, as with Postgres in OrbStack's VM while the VM is paused.
  ['never answers', () => {}],
  // As OrbStack did on a start during macOS's DarkWake.
  ['closes the connection', (socket: Socket) => socket.destroy()],
] as const) {
  void test(
    `startup waits while Postgres ${behavior}, without running ZBUGS_PG_START`,
    {timeout: 30_000},
    async t => {
      const sockets = new Set<Socket>();
      let psqlConnections = 0;
      let answering = false;
      const server = createServer(socket => {
        sockets.add(socket);
        if (answering) {
          answer(socket);
        } else {
          // psql sends its startup message; the TCP probe sends nothing.
          socket.once('data', () => {
            psqlConnections++;
            unanswered(socket);
          });
        }
      });
      const port = await listen(server);
      const {file, start} = startedFile(t);
      // sslmode=disable, so that psql makes one connection per check.
      const url = `postgresql://user@127.0.0.1:${port}/zbugs?sslmode=disable`;
      const waiting = startup(t, url, start, 50);
      t.after(() => {
        for (const socket of sockets) {
          socket.destroy();
        }
        server.close();
      });

      // A fourth check starts only after the third, which ran ZBUGS_PG_START
      // when a check that Postgres didn't answer counted.
      while (psqlConnections < 4) {
        assert.equal(waiting.settled, false, 'startup stopped waiting');
        await sleep(50);
      }
      assert.equal(existsSync(file), false);

      // The VM wakes.
      answering = true;
      await waiting.wait;
    },
  );
}

void test(
  'startup runs ZBUGS_PG_START once Postgres has refused three checks, then waits until it answers',
  {timeout: 30_000},
  async t => {
    // A port nothing listens on, as when Postgres is stopped.
    const sockets = new Set<Socket>();
    const server = createServer(socket => {
      sockets.add(socket);
      answer(socket);
    });
    const port = await listen(server);
    await new Promise(resolve => server.close(resolve));
    const {file, start} = startedFile(t);
    const interval = 200;
    const began = Date.now();
    const waiting = startup(
      t,
      `postgresql://user@127.0.0.1:${port}/zbugs?sslmode=disable`,
      start,
      interval,
    );
    t.after(() => {
      for (const socket of sockets) {
        socket.destroy();
      }
      server.close();
    });

    while (!existsSync(file)) {
      assert.ok(Date.now() - began < 10_000, 'ZBUGS_PG_START did not run');
      assert.equal(waiting.settled, false, 'startup stopped waiting');
      await sleep(20);
    }
    // The first check is right away, so the third is two intervals in.
    assert.ok(
      Date.now() - began >= 2 * interval,
      'ZBUGS_PG_START ran before the third refused check',
    );

    // Postgres starts.
    server.listen(port, '127.0.0.1');
    await waiting.wait;
  },
);

void test('a check that times out does not run ZBUGS_PG_START', async t => {
  // Accepts connections and never answers, as Postgres does in OrbStack's VM
  // while the VM is paused.
  const sockets = new Set<Socket>();
  let psqlConnections = 0;
  const server = createServer(socket => {
    sockets.add(socket);
    // psql sends its startup message; the watcher's TCP probe sends nothing.
    socket.once('data', () => psqlConnections++);
  });
  const port = await listen(server);
  const {file, start} = startedFile(t);
  // sslmode=disable, so that psql makes one connection per check.
  const url = `postgresql://user@127.0.0.1:${port}/zbugs?sslmode=disable`;
  const began = Date.now();
  const stop = watchPostgres(url, env, start, 50);
  t.after(() => {
    stop();
    for (const socket of sockets) {
      socket.destroy();
    }
    server.close();
  });

  // A fourth check starts only after the third, which ran ZBUGS_PG_START
  // when a check that timed out counted.
  while (psqlConnections < 4) {
    assert.ok(Date.now() - began < 20_000, 'the watcher stopped checking');
    await sleep(50);
  }
  assert.equal(existsSync(file), false);
});

void test('a refused connection runs ZBUGS_PG_START after three checks', async t => {
  // A port nothing listens on, as when Postgres is stopped.
  const server = createServer();
  const port = await listen(server);
  await new Promise(resolve => server.close(resolve));
  const {file, start} = startedFile(t);
  const interval = 100;
  const began = Date.now();
  const stop = watchPostgres(
    `postgresql://user@127.0.0.1:${port}/zbugs`,
    env,
    start,
    interval,
  );
  t.after(stop);

  while (!existsSync(file)) {
    assert.ok(Date.now() - began < 10_000, 'ZBUGS_PG_START did not run');
    await sleep(20);
  }
  assert.ok(
    Date.now() - began >= 3 * interval,
    'ZBUGS_PG_START ran before the third refused check',
  );
});
