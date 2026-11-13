/**
 * server.ts's Postgres checks, in their own module so that they can be
 * tested without starting the backend (postgres.test.ts).
 */
import {execFile, type ExecFileException} from 'node:child_process';
import {connect} from 'node:net';
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
  try {
    await promisify(execFile)('psql', [url, '-tAc', 'SELECT 1'], {
      env: {['PGCONNECT_TIMEOUT']: '5', ...env},
    });
    return true;
  } catch {
    return false;
  }
}

/**
 * zero-cache keeps retrying while Postgres refuses connections and catches
 * up once it answers, but nothing starts a stopped Postgres again: Docker's
 * `restart: always` leaves a container that `docker stop` stopped alone
 * until Docker itself restarts. So a kept backend runs `start` once
 * Postgres has refused three checks 10 seconds apart, longer than a Docker
 * restart takes, and again after three more.
 *
 * A check that Postgres neither answers nor refuses doesn't count. That is
 * how Postgres in OrbStack's VM looks while the VM is paused as this machine
 * sleeps: connections time out, and starting Postgres can't help, since
 * `docker` hangs until the VM wakes.
 *
 * Returns a function that stops watching.
 */
export function watchPostgres(
  url: string,
  env: NodeJS.ProcessEnv,
  start: string,
  intervalMs = 10_000,
): () => void {
  let refusals = 0;
  let stopped = false;
  let timer: NodeJS.Timeout | undefined;
  const check = async () => {
    if ((await postgresAnswers(url, env)) || !(await refuses(url))) {
      refusals = 0;
    } else if (++refusals === 3) {
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
    if (!stopped) {
      timer = setTimeout(check, intervalMs);
    }
  };
  timer = setTimeout(check, intervalMs);
  return () => {
    stopped = true;
    clearTimeout(timer);
  };
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
