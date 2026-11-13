/**
 * Tests for server.ts's Postgres watcher, against real `psql`:
 *
 *   node --test apps/zbugs-swift/server/postgres.test.ts
 */
import assert from 'node:assert/strict';
import {existsSync, mkdtempSync, rmSync} from 'node:fs';
import {createServer, type Server, type Socket} from 'node:net';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {test, type TestContext} from 'node:test';
import {watchPostgres} from './postgres.ts';

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
