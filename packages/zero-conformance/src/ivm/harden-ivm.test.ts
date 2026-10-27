// Hardening IVM vectors: deeper nesting (up to 5 levels of `related` and
// `whereExists`), junctions, `flip`/`scalar` exists, OR of EXISTS, empty IN
// lists, LIKE patterns with regex metacharacters, `start`, `limit` 0/1,
// `one()`, and pushes that edit sort keys and primary keys, applied in
// batches that flush once (as a poke does), including a remove and add of
// the same row in one batch.
//
// Writes `generated/harden-ivm-*.json` (not checked in), which
// `swift/Tests/ConformanceTests/HardenIVMTests.swift` replays. Seeded with
// `HARDEN_IVM_SEED` (default 1) and `HARDEN_IVM_SEEDS` (how many seeds from
// there, default 1), `HARDEN_IVM_CASES` cases per seed (default 2000).
//
// Numbers JSON cannot carry (NaN, ±Infinity, -0) are written as
// `{"$number": "NaN"}` and so on, in data, pushes, programs, and results.
import {test} from 'vitest';
import type {JSONValue} from '../../../shared/src/json.ts';
import type {AST, SimpleOperator} from '../../../zero-protocol/src/ast.ts';
import type {Row} from '../../../zero-protocol/src/data.ts';
import type {Format} from '../../../zero-types/src/format.ts';
import type {Schema} from '../../../zero-types/src/schema.ts';
import {buildPipeline} from '../../../zql/src/builder/builder.ts';
import {TestBuilderDelegate} from '../../../zql/src/builder/test-builder-delegate.ts';
import {ArrayView} from '../../../zql/src/ivm/array-view.ts';
import type {Change} from '../../../zql/src/ivm/change.ts';
import {MemorySource} from '../../../zql/src/ivm/memory-source.ts';
import type {
  FetchRequest,
  Input,
  InputBase,
  Operator,
  Output,
} from '../../../zql/src/ivm/operator.ts';
import {
  makeSourceChangeAdd,
  makeSourceChangeEdit,
  makeSourceChangeRemove,
} from '../../../zql/src/ivm/source.ts';
import {consume} from '../../../zql/src/ivm/stream.ts';
import {schema as testSchema} from '../../../zql/src/query/test/test-schemas.ts';
import {Random} from '../random.ts';
import {emitGeneratedVectors} from '../vectors.ts';
import {
  buildProgram,
  schemaJSON,
  type Expr,
  type Op,
  type Program,
  type Push,
  type RecordedChange,
} from './harness.ts';

const schema: Schema = testSchema;

// Special numbers survive JSON as tagged objects.
function encode(v: unknown): JSONValue {
  if (typeof v === 'number') {
    if (Number.isNaN(v)) return {$number: 'NaN'};
    if (v === Infinity) return {$number: 'Infinity'};
    if (v === -Infinity) return {$number: '-Infinity'};
    if (Object.is(v, -0)) return {$number: '-0'};
    return v;
  }
  if (v === undefined) return null;
  if (v === null || typeof v !== 'object') return v as JSONValue;
  if (Array.isArray(v)) return v.map(encode);
  const out: Record<string, JSONValue> = {};
  for (const [k, e] of Object.entries(v)) {
    if (e !== undefined) out[k] = encode(e);
  }
  return out;
}

/** What the Swift runner reads back: `encode`, then a JSON round trip. */
function roundTrip<T>(v: T): T {
  return decode(JSON.parse(JSON.stringify(encode(v)))) as T;
}

function decode(v: unknown): unknown {
  if (v === null || typeof v !== 'object') return v;
  if (Array.isArray(v)) return v.map(decode);
  const o = v as Record<string, unknown>;
  const keys = Object.keys(o);
  if (keys.length === 1 && keys[0] === '$number') {
    return Number(o.$number);
  }
  return Object.fromEntries(keys.map(k => [k, decode(o[k])]));
}

// Values. Few distinct ones so sort keys and join keys collide.
const texts = [
  'a',
  'a',
  'b',
  '',
  'A',
  '%',
  '_',
  '\\',
  'a.c',
  'a(c',
  '[a]',
  '^a$',
  'a*',
  'a+b',
  'a?',
  '{1}',
  'a|b',
  'ß',
  'SS',
  'İ',
  'i',
  'ı',
  'É',
  'é',
  'K', // Kelvin sign, folds to k
  'k',
  '\u{1f600}',
  '\ue000',
  'a\nb',
];
const plainNumbers = [
  0,
  1,
  1,
  2,
  -1,
  0.5,
  2 ** 53,
  2 ** 53 + 2,
  -(2 ** 53),
  1e21,
];
// NaN is left out: a NaN sort key breaks the indexes of JS (`node does not
// exist`) as well as Swift's.
const specialNumbers = [Infinity, -Infinity, -0];

const ids: Record<string, string[]> = {
  issue: ['i1', 'i2', 'i3', 'i4', 'i5', 'i6'],
  user: ['u1', 'u2', 'u3'],
  label: ['l1', 'l2', 'l3'],
  comment: ['c1', 'c2', 'c3', 'c4', 'c5', 'c6', 'c7'],
  revision: ['r1', 'r2', 'r3', 'r4'],
};

type Mode = {special: boolean};

function numberValue(r: Random, mode: Mode): number {
  return mode.special && r.bool(0.25)
    ? r.pick(specialNumbers)
    : r.pick(plainNumbers);
}

function columnValue(
  r: Random,
  mode: Mode,
  table: string,
  column: string,
): JSONValue {
  switch (`${table}.${column}`) {
    case 'issue.ownerId':
      return r.bool(0.3) ? null : r.pick(ids.user);
    case 'issue.closed':
      return r.bool();
    case 'issue.createdAt':
    case 'comment.createdAt':
      return numberValue(r, mode);
    case 'comment.issueId':
    case 'issueLabel.issueId':
      return r.bool(0.05) ? 'i-missing' : r.pick(ids.issue);
    case 'comment.authorId':
    case 'revision.authorId':
      return r.pick(ids.user);
    case 'revision.commentId':
      return r.pick(ids.comment);
    case 'issueLabel.labelId':
      return r.pick(ids.label);
    case 'label.name':
      // Unique in the schema: make it unique per id.
      return null;
    case 'user.metadata':
      return r.bool(0.5)
        ? null
        : {registrar: r.pick(['github', 'google']), email: r.pick(texts)};
    default:
      return r.pick(texts);
  }
}

function makeRow(r: Random, mode: Mode, table: string, id?: string): Row {
  const row: Record<string, JSONValue> = {};
  for (const column of Object.keys(schema.tables[table].columns)) {
    row[column] =
      column === 'id' && id !== undefined
        ? id
        : table === 'label' && column === 'name'
          ? `${id}-${r.pick(texts)}`
          : columnValue(r, mode, table, column);
  }
  return row as Row;
}

const pk = (table: string, row: Row) =>
  JSON.stringify(schema.tables[table].primaryKey.map(k => row[k]));

function makeData(r: Random, mode: Mode): Record<string, Row[]> {
  const data: Record<string, Row[]> = {};
  for (const table of Object.keys(schema.tables)) {
    const rows = new Map<string, Row>();
    if (table === 'issueLabel') {
      for (let i = 0; i < 9; i++) {
        const row = makeRow(r, mode, table);
        rows.set(pk(table, row), row);
      }
    } else {
      for (const id of ids[table]) {
        if (r.bool(0.7)) rows.set(id, makeRow(r, mode, table, id));
      }
    }
    data[table] = [...rows.values()];
  }
  return data;
}

/**
 * Batches of valid source changes. A batch is flushed once. Some batches
 * remove a row and add it back changed; some edits change the primary key.
 */
function makeBatches(
  r: Random,
  mode: Mode,
  data: Record<string, Row[]>,
  count: number,
): Push[][] {
  const current = new Map(
    Object.entries(data).map(([t, rows]) => [
      t,
      new Map(rows.map(row => [pk(t, row), row])),
    ]),
  );
  const tables = Object.keys(schema.tables);
  const batches: Push[][] = [];
  while (batches.length < count) {
    const batch: Push[] = [];
    const size = r.bool(0.6) ? 1 : 2 + r.int(3);
    let guard = 0;
    while (batch.length < size && guard++ < 50) {
      const table = r.pick(tables);
      const rows = current.get(table)!;
      const kind = r.int(6);
      if (kind === 0) {
        const candidates =
          table === 'issueLabel'
            ? [makeRow(r, mode, table)]
            : ids[table].map(id => makeRow(r, mode, table, id));
        const row = candidates.find(c => !rows.has(pk(table, c)));
        if (row) {
          rows.set(pk(table, row), row);
          batch.push({table, type: 'add', row});
        }
        continue;
      }
      if (rows.size === 0) continue;
      const existing = r.pick([...rows.values()]);
      const key = pk(table, existing);
      if (kind === 1) {
        rows.delete(key);
        batch.push({table, type: 'remove', row: existing});
      } else if (kind === 2) {
        // Remove and add back, changed, in one batch.
        const row = {...existing} as Record<string, JSONValue>;
        const columns = nonPK(table);
        if (columns.length > 0) {
          const column = r.pick(columns);
          row[column] = columnValue(r, mode, table, column);
        }
        rows.set(key, row as Row);
        batch.push({table, type: 'remove', row: existing});
        batch.push({table, type: 'add', row: row as Row});
      } else if (kind === 3 && table !== 'issueLabel' && r.bool(0.4)) {
        // An edit that changes the primary key.
        const free = ids[table].filter(id => !rows.has(JSON.stringify([id])));
        if (free.length === 0) continue;
        const row = {...existing, id: r.pick(free)} as Row;
        rows.delete(key);
        rows.set(pk(table, row), row);
        batch.push({table, type: 'edit', row, oldRow: existing});
      } else {
        const columns = nonPK(table);
        if (columns.length === 0) continue;
        const row = {...existing} as Record<string, JSONValue>;
        for (let i = 0, n = 1 + r.int(2); i < n; i++) {
          const column = r.pick(columns);
          row[column] = columnValue(r, mode, table, column);
        }
        rows.set(key, row as Row);
        batch.push({table, type: 'edit', row: row as Row, oldRow: existing});
      }
    }
    if (batch.length > 0) batches.push(batch);
  }
  return batches;
}

function nonPK(table: string): string[] {
  const pkCols = schema.tables[table].primaryKey;
  return Object.keys(schema.tables[table].columns).filter(
    c => !pkCols.includes(c) && !(table === 'label' && c === 'name'),
  );
}

// Queries.
const likePatterns = [
  '%',
  '_',
  'a%',
  '%b',
  'a.c',
  'a_c',
  '%(%',
  '[a]',
  '^a$',
  'a*',
  'a+b',
  'a?',
  '{1}',
  'a|b',
  '\\%',
  '\\_',
  '\\\\',
  '%\\\\%',
  'ss',
  'SS',
  'ß',
  'i',
  'İ',
  'ı',
  'é',
  'É',
  'k',
  'a%b',
  '%\u{1f600}%',
];

function comparison(
  r: Random,
  mode: Mode,
  table: string,
): [string, SimpleOperator, JSONValue] {
  const columns = Object.entries(schema.tables[table].columns);
  const [column, value] = r.pick(columns);
  if (value.type === 'json') {
    return [column, r.pick(['IS', 'IS NOT'] as const), null];
  }
  if (value.type === 'boolean') {
    const op = r.pick(['=', '!=', 'IS', 'IS NOT'] as const);
    return [column, op, op.startsWith('IS') && r.bool(0.3) ? null : r.bool()];
  }
  const domain: JSONValue[] =
    value.type === 'number'
      ? [...plainNumbers, ...(mode.special ? specialNumbers : [])]
      : [...(ids[table] ?? []), ...texts, ...ids.user, ...ids.issue];
  const op = r.pick<SimpleOperator>([
    '=',
    '!=',
    '<',
    '>',
    '<=',
    '>=',
    'IN',
    'NOT IN',
    'IS',
    'IS NOT',
    ...(value.type === 'string'
      ? (['LIKE', 'NOT LIKE', 'ILIKE', 'NOT ILIKE'] as const)
      : []),
  ]);
  switch (op) {
    case 'LIKE':
    case 'NOT LIKE':
    case 'ILIKE':
    case 'NOT ILIKE':
      return [column, op, r.pick(likePatterns)];
    case 'IN':
    case 'NOT IN': {
      const n = r.pick([0, 0, 1, 2, 3]);
      return [column, op, Array.from({length: n}, () => r.pick(domain))];
    }
    case 'IS':
    case 'IS NOT':
      return [column, op, r.bool(0.6) ? null : r.pick(domain)];
    default:
      return [column, op, r.pick(domain)];
  }
}

type Ctx = {
  depth: number;
  maxDepth: number;
  inJunction: boolean;
  inExists: boolean;
  mode: Mode;
};

const rels = (table: string) =>
  Object.entries(schema.relationships[table] ?? {});

function existsOp(r: Random, table: string, ctx: Ctx) {
  const all = rels(table);
  if (all.length === 0 || ctx.depth >= ctx.maxDepth) return undefined;
  const [rel, hops] = r.pick(all);
  const dest = hops.at(-1)!.destSchema;
  const sub = r.bool(0.7)
    ? subOps(r, dest, {
        ...ctx,
        depth: ctx.depth + 1,
        inJunction: hops.length > 1,
        inExists: true,
      })
    : undefined;
  return {
    rel,
    sub,
    flip: r.bool(0.4) ? r.bool() : undefined,
    scalar: r.bool(0.15) ? true : undefined,
  };
}

function expr(
  r: Random,
  table: string,
  ctx: Ctx,
  depth = 0,
  negated = false,
): Expr {
  const k = r.int(depth > 2 ? 3 : 7);
  if (k <= 1) return {cmp: comparison(r, ctx.mode, table)};
  if (k === 2 && (!negated || r.bool(0.1))) {
    // A negated EXISTS is rejected by the client builder: keep a few.
    const e = existsOp(r, table, ctx);
    if (e) return {exists: e.rel, sub: e.sub, flip: e.flip, scalar: e.scalar};
    return {cmp: comparison(r, ctx.mode, table)};
  }
  if (k === 6) return {not: expr(r, table, ctx, depth + 1, true)};
  const n = r.pick([0, 1, 2, 2, 3]);
  const parts = Array.from({length: n}, () =>
    expr(r, table, ctx, depth + 1, negated),
  );
  return r.bool() ? {and: parts} : {or: parts};
}

function subOps(r: Random, table: string, ctx: Ctx): Op[] {
  const ops = randomOps(r, table, ctx, r.int(4));
  // Most subqueries go one level deeper, so chains reach `maxDepth`.
  if (ctx.depth < ctx.maxDepth && r.bool(0.75)) {
    const deeper = deepOp(r, table, ctx);
    if (deeper) ops.splice(r.int(ops.length + 1), 0, deeper);
  }
  return ops;
}

/** A `related` or `whereExists` op whose subquery nests further. */
function deepOp(r: Random, table: string, ctx: Ctx): Op | undefined {
  if (!ctx.inExists && r.bool(0.5)) {
    const all = rels(table);
    if (all.length === 0) return undefined;
    const [rel, hops] = r.pick(all);
    const dest = hops.at(-1)!.destSchema;
    return {
      op: 'related',
      rel,
      sub: subOps(r, dest, {
        ...ctx,
        depth: ctx.depth + 1,
        inJunction: hops.length > 1,
      }),
    };
  }
  const e = existsOp(r, table, ctx);
  return e && {op: 'whereExists', ...e};
}

function randomOps(
  r: Random,
  table: string,
  ctx: Ctx,
  count: number,
  data?: Record<string, Row[]>,
): Op[] {
  const ops: Op[] = [];
  for (let i = 0; i < count; i++) {
    const k = r.int(12);
    if (k <= 1) {
      const c = comparison(r, ctx.mode, table);
      ops.push(
        c[1] === '=' && r.bool()
          ? {op: 'where', field: c[0], value: c[2]}
          : {op: 'where', field: c[0], cmp: c[1], value: c[2]},
      );
    } else if (k === 2) {
      ops.push({op: 'whereExpr', expr: expr(r, table, ctx)});
    } else if (k === 3) {
      const e = existsOp(r, table, ctx);
      if (e) ops.push({op: 'whereExists', ...e});
    } else if (k === 4 || k === 5) {
      // OR of EXISTS, the shape that splits into UnionFanOut/FanIn.
      if (k === 5) {
        const a = existsOp(r, table, ctx);
        const b = existsOp(r, table, ctx);
        if (a && b) {
          ops.push({
            op: 'whereExpr',
            expr: {
              or: [
                {exists: a.rel, sub: a.sub, flip: a.flip, scalar: a.scalar},
                r.bool()
                  ? {exists: b.rel, sub: b.sub, flip: b.flip, scalar: b.scalar}
                  : {cmp: comparison(r, ctx.mode, table)},
              ],
            },
          });
        }
        continue;
      }
      if (ctx.inExists && !r.bool(0.1)) continue;
      if (ctx.depth >= ctx.maxDepth) continue;
      const all = rels(table);
      if (all.length === 0) continue;
      const [rel, hops] = r.pick(all);
      const dest = hops.at(-1)!.destSchema;
      ops.push({
        op: 'related',
        rel,
        sub: r.bool(0.8)
          ? subOps(r, dest, {
              ...ctx,
              depth: ctx.depth + 1,
              inJunction: hops.length > 1,
            })
          : undefined,
      });
    } else if (k === 6 || k === 7) {
      if (ctx.inJunction) continue;
      const columns = Object.entries(schema.tables[table].columns)
        .filter(([, v]) => v.type !== 'json')
        .map(([c]) => c);
      ops.push({
        op: 'orderBy',
        field: r.pick(columns),
        dir: r.pick(['asc', 'desc']),
      });
    } else if (k === 8) {
      if (ctx.inJunction) continue;
      ops.push({op: 'limit', n: r.pick([0, 1, 1, 2, 3])});
    } else if (k === 9) {
      if (ctx.inJunction || ctx.depth !== 0 || !data) continue;
      const rows = data[table];
      if (rows.length === 0) continue;
      const row = r.pick(rows);
      ops.push({
        op: 'start',
        row: r.bool(0.2) ? {...row, id: `${String(row.id)}x`} : row,
        inclusive: r.bool(0.6) ? r.bool() : undefined,
      });
    } else if (k === 10) {
      if (ctx.inJunction || ctx.inExists) continue;
      ops.push({op: 'one'});
    }
  }
  return ops;
}

function makeProgram(
  r: Random,
  mode: Mode,
  data: Record<string, Row[]>,
): Program {
  const table = r.pick([
    'issue',
    'issue',
    'comment',
    'user',
    'label',
    'revision',
  ]);
  const ctx: Ctx = {
    depth: 0,
    maxDepth: r.pick([2, 3, 4, 5, 5]),
    inJunction: false,
    inExists: false,
    mode,
  };
  const program: Program = {
    table,
    ops: randomOps(r, table, ctx, 1 + r.int(6), data),
  };
  // Mostly a chain that nests to `maxDepth`.
  if (r.bool(0.7)) {
    const deeper = deepOp(r, table, ctx);
    if (deeper) program.ops.splice(r.int(program.ops.length + 1), 0, deeper);
  }
  return program;
}

// Running.

function record(change: Change): RecordedChange {
  switch (change[0]) {
    case 0:
      return {type: 'add', row: change[1].row};
    case 1:
      return {type: 'remove', row: change[1].row};
    case 2:
      return {type: 'edit', row: change[1].row, oldRow: change[2].row};
    case 3:
      return {
        type: 'child',
        row: change[1].row,
        child: {
          relationshipName: change[2].relationshipName,
          change: record(change[2].change),
        },
      };
  }
}

class Recorder implements Operator {
  readonly #input: Input;
  #output: Output | undefined;
  readonly changes: RecordedChange[] = [];
  constructor(input: Input) {
    this.#input = input;
    input.setOutput(this);
  }
  setOutput(output: Output): void {
    this.#output = output;
  }
  getSchema() {
    return this.#input.getSchema();
  }
  destroy(): void {
    this.#input.destroy();
  }
  fetch(req: FetchRequest) {
    return this.#input.fetch(req);
  }
  *push(change: Change, _pusher: InputBase) {
    this.changes.push(record(change));
    yield* this.#output!.push(change, this);
  }
  *reconcile(_pusher: InputBase) {
    if (this.#output?.reconcile) yield* this.#output.reconcile(this);
  }
}

type Step = {changes: JSONValue; data: JSONValue; notified: boolean};
/** `batch` is set for an error thrown by a push rather than by hydrating. */
type Result =
  | {error: string; batch?: number}
  | {initial: JSONValue; steps: Step[]};

function errorText(e: unknown) {
  return String(e instanceof Error ? e.message : e);
}

/**
 * Hydrates a view, applies each batch of pushes, flushes once per batch, and
 * records the changes, the view, and whether listeners were notified.
 */
function runBatches(
  data: Record<string, Row[]>,
  ast: AST,
  format: Format,
  batches: readonly Push[][],
): Result {
  const sources: Record<string, MemorySource> = {};
  for (const [name, table] of Object.entries(schema.tables)) {
    const source = new MemorySource(name, table.columns, table.primaryKey);
    for (const row of data[name] ?? []) {
      consume(source.push(makeSourceChangeAdd(row)));
    }
    sources[name] = source;
  }
  let recorder: Recorder;
  let view: ArrayView<never>;
  let notified = 0;
  try {
    const input = buildPipeline(ast, new TestBuilderDelegate(sources), 'q');
    recorder = new Recorder(input);
    view = new ArrayView(recorder, format, true, () => {});
    view.addListener(() => notified++);
  } catch (e) {
    return {error: errorText(e)};
  }
  const initial = encode(view.data);
  const steps: Step[] = [];
  for (const [i, batch] of batches.entries()) {
    recorder.changes.length = 0;
    notified = 0;
    try {
      for (const push of batch) {
        const change =
          push.type === 'edit'
            ? makeSourceChangeEdit(push.row, push.oldRow!)
            : push.type === 'add'
              ? makeSourceChangeAdd(push.row)
              : makeSourceChangeRemove(push.row);
        consume(sources[push.table].push(change));
      }
    } catch (e) {
      return {error: errorText(e), batch: i};
    }
    view.flush();
    steps.push({
      changes: encode(recorder.changes),
      data: encode(view.data),
      notified: notified > 0,
    });
  }
  return {initial, steps};
}

type Dataset = {data: Record<string, Row[]>; batches: Push[][]};

function generate(
  name: string,
  seeds: number[],
  casesPerSeed: number,
  mode: Mode,
) {
  const sets: Dataset[] = [];
  const cases: JSONValue[] = [];
  let errors = 0;
  for (const seed of seeds) {
    const r = new Random(seed);
    const first = sets.length;
    for (let i = 0; i < 8; i++) {
      const data = roundTrip(makeData(r, mode));
      sets.push({data, batches: roundTrip(makeBatches(r, mode, data, 30))});
    }
    let made = 0;
    let attempts = 0;
    while (made < casesPerSeed && attempts++ < casesPerSeed * 8) {
      const dataset = first + r.int(8);
      const program = roundTrip(makeProgram(r, mode, sets[dataset].data));
      let built;
      try {
        built = buildProgram(schema, program);
      } catch {
        continue;
      }
      const result = runBatches(
        sets[dataset].data,
        built.ast,
        built.format,
        sets[dataset].batches,
      );
      if ('error' in result) {
        // Keep a few, so errors are compared too.
        if (errors++ % 10 !== 0) continue;
      }
      made++;
      cases.push(
        encode({
          name: `seed ${seed} case ${attempts}`,
          dataset,
          program,
          ast: built.ast,
          format: built.format,
          result,
        }),
      );
    }
  }
  emitGeneratedVectors(
    name,
    'ivm-batches',
    'Hardening: deep nesting, junctions, flip/scalar exists, batched pushes with one flush each.',
    cases,
    {schema: schemaJSON(schema), datasets: encode(sets)},
  );
}

const seed = Number(process.env.HARDEN_IVM_SEED ?? 1);
const seedCount = Number(process.env.HARDEN_IVM_SEEDS ?? 1);
const casesPerSeed = Number(process.env.HARDEN_IVM_CASES ?? 2000);
const seeds = Array.from({length: seedCount}, (_, i) => seed + i);

test('harden ivm fuzz', {timeout: 3_600_000}, () => {
  generate('harden-ivm-fuzz', seeds, casesPerSeed, {special: false});
});

test('harden ivm special numbers', {timeout: 3_600_000}, () => {
  generate('harden-ivm-special', seeds, Math.ceil(casesPerSeed / 4), {
    special: true,
  });
});
