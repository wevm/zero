/**
 * server.ts's Postgres checks, in their own module so that they can be
 * tested without starting the backend (postgres.test.ts).
 */
import {execFile, type ExecFileException} from 'node:child_process';
import {connect} from 'node:net';
import {setTimeout as sleep} from 'node:timers/promises';
import {promisify} from 'node:util';

const START_TIMEOUT_MS = 60_000;

/**
 * Whether Postgres at `url` answers a query, within `PGCONNECT_TIMEOUT`
 * seconds (5 unless `env` sets it).
 */
export async function postgresAnswers(
  url: string,
  env: NodeJS.ProcessEnv,
): Promise<boolean> {
  return (await psqlError(url, env)) === undefined;
}

/**
 * Resolves once Postgres at `url` answers, checking every `intervalMs`, and
 * logs what it waits on each time that changes.
 *
 * zero-cache keeps retrying while Postgres refuses connections and catches
 * up once it answers, but nothing starts a stopped Postgres again: Docker's
 * `restart: always` leaves a container that `docker stop` stopped alone
 * until Docker itself restarts. So with `start`, it runs `start` once
 * Postgres has refused three checks in a row, 10 seconds apart, longer than
 * a Docker restart takes, and again after three more.
 *
 * A check that Postgres neither answers nor refuses doesn't count. That is
 * how Postgres in OrbStack's VM looks while the VM is paused as this machine
 * sleeps: connections time out, or OrbStack closes them, and starting
 * Postgres can't help, since `docker` hangs until the VM wakes.
 *
 * Rejects once `signal` aborts.
 */
export async function waitForPostgres(
  url: string,
  env: NodeJS.ProcessEnv,
  start: string | undefined,
  intervalMs = 10_000,
  signal?: AbortSignal,
): Promise<void> {
  let refusals = 0;
  let waitingOn: string | undefined;
  for (;;) {
    const error = await psqlError(url, env);
    signal?.throwIfAborted();
    if (error === undefined) {
      if (waitingOn !== undefined) {
        process.stderr.write('Postgres answers\n');
      }
      return;
    }
    if (error !== waitingOn) {
      waitingOn = error;
      process.stderr.write(`waiting for Postgres: ${error}\n`);
    }
    if (!(await refuses(url))) {
      refusals = 0;
    } else if (start && ++refusals === 3) {
      refusals = 0;
      process.stderr.write(
        'Postgres refuses connections; running ZBUGS_PG_START\n',
      );
      try {
        await promisify(execFile)('/bin/sh', ['-c', start], {
          env,
          timeout: START_TIMEOUT_MS,
        });
      } catch (e) {
        process.stderr.write(`ZBUGS_PG_START ${startFailure(e)}\n`);
      }
    }
    await sleep(intervalMs, undefined, {signal});
  }
}

/**
 * Checks Postgres every `intervalMs` and, while it doesn't answer, waits for
 * it as waitForPostgres does, running `start` when it refuses connections.
 *
 * Returns a function that stops watching.
 */
export function watchPostgres(
  url: string,
  env: NodeJS.ProcessEnv,
  start: string,
  intervalMs = 10_000,
): () => void {
  const stopping = new AbortController();
  const {signal} = stopping;
  void (async () => {
    for (;;) {
      await sleep(intervalMs, undefined, {signal});
      await waitForPostgres(url, env, start, intervalMs, signal);
    }
  })().catch(e => {
    if (!signal.aborted) {
      throw e;
    }
  });
  return () => stopping.abort();
}

/**
 * The first line of psql's error when Postgres at `url` doesn't answer a
 * query within `PGCONNECT_TIMEOUT` seconds (5 unless `env` sets it), or
 * undefined when it answers.
 */
async function psqlError(
  url: string,
  env: NodeJS.ProcessEnv,
): Promise<string | undefined> {
  try {
    await promisify(execFile)('psql', [url, '-tAc', 'SELECT 1'], {
      env: {['PGCONNECT_TIMEOUT']: '5', ...env},
    });
    return undefined;
  } catch (e) {
    const {message, stderr} = e as Error & {stderr?: string};
    return (stderr?.trim() || message).split('\n')[0];
  }
}

/** Whether the host and port of `url` refuse a TCP connection. */
function refuses(url: string): Promise<boolean> {
  const {hostname, port} = new URL(url);
  return new Promise(resolve => {
    const socket = connect({
      host: hostname,
      port: Number(port || 5432),
      timeout: 5000,
    });
    socket.once('connect', () => {
      socket.destroy();
      resolve(false);
    });
    socket.once('timeout', () => {
      socket.destroy();
      resolve(false);
    });
    socket.once('error', e => {
      resolve((e as NodeJS.ErrnoException).code === 'ECONNREFUSED');
    });
  });
}

/** How the start command failed: timed out, killed, or exited with a code. */
function startFailure(e: unknown): string {
  const {killed, signal, code, stderr} = e as ExecFileException & {
    stderr?: string;
  };
  const output = stderr?.trim() ? `: ${stderr.trim()}` : '';
  if (killed) {
    return `timed out after ${START_TIMEOUT_MS / 1000} s${output}`;
  }
  if (signal) {
    return `was killed by ${signal}${output}`;
  }
  return `failed with exit code ${code}${output}`;
}
