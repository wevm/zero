// Sets the dependencies of the restored packages' package.json files to the
// specifiers pnpm-lock.yaml has for them. rocicorp/mono no longer publishes
// these manifests, but its lockfile still has their importers, and an upstream
// merge that bumps one makes `pnpm install --frozen-lockfile` fail until the
// manifest follows. See README.md.
//
// Usage: node tools/restored-tests/align-manifests.mjs
import {readFileSync, writeFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';

const root = fileURLToPath(new URL('../../', import.meta.url));
const packages = ['packages/zql-integration-tests', 'packages/zqlite-zql-test'];
const groups = ['dependencies', 'devDependencies', 'optionalDependencies'];
// The lines of an importer: a group, a dependency, and its specifier.
const groupLine = /^ {4}(\w+):$/;
const nameLine = /^ {6}('?)(.+)\1:$/;
const specifierLine = /^ {8}specifier: (.+)$/;
const quoted = /^'(.*)'$/;

/** The `{group: {name: specifier}}` of one importer in pnpm-lock.yaml. */
function importer(lock, dir) {
  const lines = lock.split('\n');
  const start = lines.indexOf(`  ${dir}:`);
  if (start === -1) {
    throw new Error(`pnpm-lock.yaml has no importer for ${dir}`);
  }
  const deps = {};
  let group;
  let name;
  for (const line of lines.slice(start + 1)) {
    if (!line.startsWith('    ')) {
      break;
    }
    let m;
    if ((m = groupLine.exec(line))) {
      group = m[1];
      deps[group] = {};
    } else if ((m = nameLine.exec(line))) {
      name = m[2];
    } else if ((m = specifierLine.exec(line))) {
      deps[group][name] = m[1].replace(quoted, '$1');
    }
  }
  return deps;
}

const lock = readFileSync(root + 'pnpm-lock.yaml', 'utf8');
for (const dir of packages) {
  const path = `${root}${dir}/package.json`;
  const text = readFileSync(path, 'utf8');
  const pkg = JSON.parse(text);
  const deps = importer(lock, dir);
  for (const group of groups) {
    if (deps[group]) {
      pkg[group] = deps[group];
    } else {
      delete pkg[group];
    }
  }
  const next = JSON.stringify(pkg, null, 2) + '\n';
  if (next !== text) {
    writeFileSync(path, next);
    process.stdout.write(`updated ${dir}/package.json\n`);
  }
}
