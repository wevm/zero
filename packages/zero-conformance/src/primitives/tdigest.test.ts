import {test} from 'vitest';
import {TDigest} from '../../../shared/src/tdigest.ts';
import {Random} from '../random.ts';
import {emitVectors} from '../vectors.ts';

/** A number as JSON: NaN and the infinities as strings. */
type Num = number | 'NaN' | 'Infinity' | '-Infinity';

function enc(n: number): Num {
  if (Number.isNaN(n)) return 'NaN';
  if (n === Infinity) return 'Infinity';
  if (n === -Infinity) return '-Infinity';
  return n;
}

function dec(n: Num): number {
  return typeof n === 'number' ? n : Number(n);
}

type Op =
  | {op: 'add'; d: number; values: [Num, Num][]}
  | {op: 'merge'; d: number; from: number}
  | {op: 'quantile'; d: number; q: Num[]}
  | {op: 'cdf'; d: number; x: Num[]}
  | {op: 'count'; d: number}
  | {op: 'toJSON'; d: number}
  | {op: 'fromJSON'; d: number; json: number[]}
  | {op: 'reset'; d: number};

type Case = {name: string; compressions: number[]; ops: Op[]};

const QUANTILES: Num[] = [
  0,
  0.001,
  0.01,
  0.1,
  0.25,
  0.5,
  0.75,
  0.9,
  0.95,
  0.99,
  0.999,
  1,
  -0.1,
  1.1,
  'NaN',
];

function run(c: Case): unknown[] {
  const digests = c.compressions.map(n => new TDigest(n));
  return c.ops.map(op => {
    const d = digests[op.d];
    switch (op.op) {
      case 'add':
        for (const [m, w] of op.values) d.add(dec(m), dec(w));
        return null;
      case 'merge':
        d.merge(digests[op.from]);
        return null;
      case 'quantile':
        return op.q.map(q => enc(d.quantile(dec(q))));
      case 'cdf':
        return op.x.map(x => enc(d.cdf(dec(x))));
      case 'count':
        return enc(d.count());
      case 'toJSON':
        return d.toJSON().map(enc);
      case 'fromJSON':
        try {
          digests[op.d] = TDigest.fromJSON(
            op.json as unknown as Parameters<typeof TDigest.fromJSON>[0],
          );
          return 'ok';
        } catch (e) {
          return {threw: (e as Error).message};
        }
      case 'reset':
        d.reset();
        return null;
    }
  });
}

function values(r: Random, n: number, kind: string): [Num, Num][] {
  const out: [Num, Num][] = [];
  for (let i = 0; i < n; i++) {
    let m: number;
    switch (kind) {
      case 'ints':
        m = r.int(20);
        break;
      case 'timing':
        // Durations in ms, mostly small with a long tail.
        m = Math.round(-Math.log(1 - r.next()) * 30 * 1000) / 1000;
        break;
      case 'wide':
        m = Math.round((r.next() - 0.5) * 2e7) / 10;
        break;
      default:
        m = Math.round(r.next() * 1e6) / 1000;
    }
    let w: number;
    const p = r.next();
    if (p < 0.5) w = 1;
    else if (p < 0.7) w = 32;
    else if (p < 0.95) w = Math.round(r.next() * 1000) / 100 + 0.01;
    else w = r.pick([0, -1, NaN, Infinity]);
    out.push([enc(r.next() < 0.01 ? NaN : m), enc(w)]);
  }
  return out;
}

test('tdigest', () => {
  const cases: Case[] = [
    {
      name: 'empty',
      compressions: [100],
      ops: [
        {op: 'count', d: 0},
        {op: 'quantile', d: 0, q: QUANTILES},
        {op: 'cdf', d: 0, x: [0, 1]},
        {op: 'toJSON', d: 0},
      ],
    },
    {
      name: 'one value',
      compressions: [100],
      ops: [
        {op: 'add', d: 0, values: [[5, 1]]},
        {op: 'quantile', d: 0, q: QUANTILES},
        {op: 'cdf', d: 0, x: [4, 5, 6]},
        {op: 'toJSON', d: 0},
      ],
    },
    {
      name: 'two values, sampled weight',
      compressions: [100],
      ops: [
        {
          op: 'add',
          d: 0,
          values: [
            [0, 32],
            [3, 32],
          ],
        },
        {op: 'quantile', d: 0, q: QUANTILES},
        {op: 'cdf', d: 0, x: [-1, 0, 1, 1.5, 3, 4]},
        {op: 'count', d: 0},
        {op: 'toJSON', d: 0},
      ],
    },
    {
      name: 'ignored values',
      compressions: [100],
      ops: [
        {
          op: 'add',
          d: 0,
          values: [
            ['NaN', 1],
            [1, 0],
            [1, -1],
            [1, 'NaN'],
            [1, 'Infinity'],
            [2, 1],
            [-0, 1],
            [0, 1],
          ],
        },
        {op: 'count', d: 0},
        {op: 'quantile', d: 0, q: QUANTILES},
        {op: 'toJSON', d: 0},
      ],
    },
    {
      name: 'fromJSON',
      compressions: [100],
      ops: [
        {op: 'fromJSON', d: 0, json: [100, 1, 2, 3, 4, 10, 1]},
        {op: 'quantile', d: 0, q: QUANTILES},
        {op: 'toJSON', d: 0},
        {op: 'fromJSON', d: 0, json: [100, 1]},
        {op: 'fromJSON', d: 0, json: [50]},
        {op: 'count', d: 0},
        {op: 'toJSON', d: 0},
        {op: 'fromJSON', d: 0, json: [1000, 5, 1, 2, 1]},
        {op: 'quantile', d: 0, q: QUANTILES},
        {op: 'toJSON', d: 0},
      ],
    },
    {
      name: 'merge into itself',
      compressions: [100],
      ops: [
        {
          op: 'add',
          d: 0,
          values: [
            [1, 1],
            [2, 1],
            [3, 1],
          ],
        },
        {op: 'merge', d: 0, from: 0},
        {op: 'count', d: 0},
        {op: 'quantile', d: 0, q: QUANTILES},
        {op: 'toJSON', d: 0},
      ],
    },
  ];

  const r = new Random(0x7d16);
  const kinds = ['uniform', 'ints', 'timing', 'wide'];
  const compressions = [100, 100, 100, 20, 5, 1000, 33.5];
  for (let i = 0; i < 30; i++) {
    const n = r.pick([1, 2, 3]);
    const cs = Array.from({length: n}, () => r.pick(compressions));
    const ops: Op[] = [];
    const steps = 2 + r.int(6);
    for (let s = 0; s < steps; s++) {
      const d = r.int(n);
      const p = r.next();
      if (p < 0.55) {
        const size = r.pick([1, 5, 50, 300, 850]);
        ops.push({op: 'add', d, values: values(r, size, r.pick(kinds))});
      } else if (p < 0.7 && n > 1) {
        ops.push({op: 'merge', d, from: r.int(n)});
      } else if (p < 0.8) {
        ops.push({op: 'quantile', d, q: QUANTILES});
      } else if (p < 0.85) {
        ops.push({op: 'cdf', d, x: [0, 1, 10, 100, 500, 999]});
      } else if (p < 0.95) {
        ops.push({op: 'toJSON', d});
      } else {
        ops.push({op: 'reset', d});
      }
    }
    for (let d = 0; d < n; d++) {
      ops.push(
        {op: 'count', d},
        {op: 'quantile', d, q: QUANTILES},
        {op: 'cdf', d, x: [0, 1, 10, 100, 500, 999]},
        {op: 'toJSON', d},
      );
    }
    cases.push({name: `random ${i}`, compressions: cs, ops});
  }

  // The JSON round trip: each final digest read back.
  for (const c of cases.slice()) {
    const results = run(c);
    const json = c.ops.flatMap((op, i) =>
      op.op === 'toJSON' && results[i] !== null ? [results[i] as Num[]] : [],
    );
    const last = json.at(-1);
    if (last && last.every(n => typeof n === 'number')) {
      cases.push({
        name: `${c.name}, read back`,
        compressions: [100],
        ops: [
          {op: 'fromJSON', d: 0, json: last as number[]},
          {op: 'quantile', d: 0, q: QUANTILES},
          {op: 'toJSON', d: 0},
        ],
      });
    }
  }

  emitVectors(
    'tdigest',
    'tdigest',
    'TDigest (shared/src/tdigest.ts): each case runs `ops` on `compressions.length` digests; `expect` holds each op result (null for add, merge, and reset). NaN and the infinities are strings.',
    cases.map(c => ({...c, expect: run(c)})),
    undefined,
    {compact: true},
  );
});
