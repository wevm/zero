/**
 * Runs zbugs' backend for the Swift app, in development and in the
 * end-to-end tests: a fresh Postgres database with zbugs' migrations and
 * seed data, zbugs' API server (apps/zbugs/api), and zero-cache, all from
 * this repository's sources.
 *
 *   node apps/zbugs-swift/server/server.ts
 *
 * Needs Node 24 and `psql` on the PATH, and Postgres with
 * `wal_level = logical`. Prints a token for the seeded crew user, then
 * `ready <zero-cache URL> <API URL>` once both serve, and stops everything
 * on SIGINT or SIGTERM.
 *
 * Environment:
 *   ZBUGS_PG         admin connection URL
 *                    (default postgresql://postgres@127.0.0.1:5432/postgres)
 *   ZBUGS_DB         database to (re)create (default zbugs_swift)
 *   ZBUGS_SEED       seed data directory in apps/zbugs/db/seed-data
 *                    (default github)
 *   ZBUGS_HOST       interface to listen on (default 127.0.0.1)
 *   ZBUGS_PORT       zero-cache port (default 4848)
 *   ZBUGS_API_PORT   API server port (default 5173)
 *   ZBUGS_DIR        working directory for the replica (default a temp dir)
 */
import {execFileSync, spawn} from 'node:child_process';
import {mkdtempSync, readdirSync} from 'node:fs';
import {registerHooks} from 'node:module';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {fileURLToPath, pathToFileURL} from 'node:url';
import {createJwkPair} from '../../../packages/zero-cache/src/auth/jwt.ts';

const env = process.env;
const adminURL =
  env['ZBUGS_PG'] ?? 'postgresql://postgres@127.0.0.1:5432/postgres';
const dbName = env['ZBUGS_DB'] ?? 'zbugs_swift';
const host = env['ZBUGS_HOST'] ?? '127.0.0.1';
const port = Number(env['ZBUGS_PORT'] ?? 4848);
const apiPort = Number(env['ZBUGS_API_PORT'] ?? 5173);
const workDir = env['ZBUGS_DIR'] ?? mkdtempSync(join(tmpdir(), 'zbugs-swift-'));

const here = (path: string) => fileURLToPath(new URL(path, import.meta.url));
const zbugs = here('../../zbugs/');
const upstreamURL = (() => {
  const url = new URL(adminURL);
  url.pathname = `/${dbName}`;
  return url.toString();
})();

function psql(url: string, ...args: string[]) {
  execFileSync('psql', [url, '-v', 'ON_ERROR_STOP=1', '-q', ...args], {
    stdio: ['ignore', 'ignore', 'inherit'],
  });
}

// The database: zbugs' migrations, then its seed script.
psql(adminURL, '-c', `DROP DATABASE IF EXISTS ${dbName} WITH (FORCE)`);
psql(adminURL, '-c', `CREATE DATABASE ${dbName}`);
const migrations = join(zbugs, 'db/migrations');
for (const file of readdirSync(migrations)
  .filter(f => f.endsWith('.sql'))
  .sort()) {
  // In a transaction each, as drizzle-kit applies them.
  psql(upstreamURL, '--single-transaction', '-f', join(migrations, file));
}
execFileSync(process.execPath, [join(zbugs, 'scripts/seed.ts')], {
  cwd: zbugs,
  stdio: ['ignore', 'ignore', 'inherit'],
  env: {
    ...env,
    ['ZERO_UPSTREAM_DB']: upstreamURL,
    ['ZERO_SEED_DATA_DIR']: join(
      zbugs,
      'db/seed-data',
      env['ZBUGS_SEED'] ?? 'github',
    ),
  },
});
const crewID = execFileSync(
  'psql',
  [
    upstreamURL,
    '-tAc',
    `SELECT id FROM "user" WHERE role = 'crew' ORDER BY login LIMIT 1`,
  ],
  {encoding: 'utf8'},
).trim();

// The API server, as `pnpm dev` runs it in apps/zbugs, with keys of its own.
const {privateJwk, publicJwk} = await createJwkPair();
env['ZERO_UPSTREAM_DB'] = upstreamURL;
env['PRIVATE_JWK'] = JSON.stringify(privateJwk);
env['VITE_PUBLIC_JWK'] = JSON.stringify(publicJwk);
const zeroSources: Record<string, string> = {
  '@rocicorp/zero': 'zero.ts',
  '@rocicorp/zero/server': 'server.ts',
  '@rocicorp/zero/server/adapters/postgresjs': 'adapters/postgresjs.ts',
};
// What Vite does for apps/zbugs: `@rocicorp/zero` is its sources, and a
// relative `.js` import may name a `.ts` file.
registerHooks({
  resolve(specifier, context, next) {
    const source = zeroSources[specifier];
    if (source) {
      return {
        url: pathToFileURL(here(`../../../packages/zero/src/${source}`)).href,
        shortCircuit: true,
      };
    }
    try {
      return next(specifier, context);
    } catch (e) {
      if (specifier.startsWith('.') && specifier.endsWith('.js')) {
        return next(specifier.slice(0, -3) + '.ts', context);
      }
      throw e;
    }
  },
});
const {fastify} = (await import('../../zbugs/api/index.ts')) as {
  fastify: {
    listen(o: {port: number; host: string}): Promise<string>;
    close(): Promise<void>;
  };
};
await fastify.listen({port: apiPort, host});

const zeroCache = spawn(
  process.execPath,
  [here('../../../packages/zero-cache/src/server/runner/main.ts')],
  {
    stdio: ['ignore', 'inherit', 'inherit'],
    env: {
      ...env,
      ['NODE_ENV']: 'development',
      ['ZERO_UPSTREAM_DB']: upstreamURL,
      ['ZERO_REPLICA_FILE']: join(workDir, 'replica.db'),
      ['ZERO_PORT']: String(port),
      ['ZERO_QUERY_URL']: `http://${host}:${apiPort}/api/query`,
      ['ZERO_MUTATE_URL']: `http://${host}:${apiPort}/api/mutate`,
      ['ZERO_NUM_SYNC_WORKERS']: '1',
      ['ZERO_LOG_LEVEL']: env['ZERO_LOG_LEVEL'] ?? 'info',
    },
  },
);

function stop(code: number) {
  zeroCache.kill('SIGTERM');
  void fastify.close();
  process.exit(code);
}
zeroCache.on('exit', code => {
  process.stderr.write(`zero-cache exited with ${code}\n`);
  stop(1);
});
process.on('SIGINT', () => stop(0));
process.on('SIGTERM', () => stop(0));

for (let i = 0; ; i++) {
  try {
    const res = await fetch(`http://${host}:${port}/`);
    if (res.ok) {
      break;
    }
  } catch {
    // Not listening yet.
  }
  if (i > 600) {
    process.stderr.write('zero-cache did not start\n');
    stop(1);
  }
  await new Promise(resolve => setTimeout(resolve, 100));
}

// The token a client logs in with: the crew user's, as `pnpm dev-token`
// mints it.
const token = execFileSync(
  process.execPath,
  [join(zbugs, 'scripts/dev-token.ts'), crewID, 'crew'],
  {
    cwd: zbugs,
    encoding: 'utf8',
  },
).trim();
process.stdout.write(`token ${token}\n`);
process.stdout.write(`ready ws://${host}:${port} http://${host}:${apiPort}\n`);
