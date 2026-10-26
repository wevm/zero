/**
 * Differential fuzz corpora for the Swift client's untrusted-input paths:
 * JSON texts mutated character by character (`JSON.parse` acceptance and
 * `JSON.stringify` of the result), downstream messages mutated as text, and
 * binary poke chunks mutated byte by byte. The Swift side must accept and
 * reject exactly what JS does, and never trap.
 */
import {readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {test} from 'vitest';
import * as v from '../../../shared/src/valita.ts';
import {downstreamSchema} from '../../../zero-protocol/src/down.ts';
import {
  POKE_CHUNK_MESSAGE_TYPE,
  pokePartsSchema,
} from '../../../zero-protocol/src/poke.ts';
import {Random} from '../random.ts';
import {emitGeneratedVectors, emitVectors} from '../vectors.ts';

// Seeds: change one to get a new corpus.
const JSON_SEED = 0x4a50e;
const JSON_FUZZ_SEED = 0x4a50f;
const DOWNSTREAM_SEED = 0xd0e7;
const POKE_SEED = 0xc4c7;

const ATOMS = [
  '0',
  '-0',
  '1',
  '-1',
  '1.5',
  '1e400',
  '-1e400',
  '1e-400',
  '2.2250738585072011e-308',
  '9007199254740993',
  '-9223372036854775808',
  '123456789012345678901234567890',
  '1E+2',
  'true',
  'false',
  'null',
  '""',
  '"a"',
  '"\\u00e9"',
  '"e\\u0301"',
  '"\\ud800"',
  '"\\udc00"',
  '"\\ud800\\ud800"',
  '"\\udc00\\ud800"',
  '"\\ud83d\\ude00"',
  '"\\u0000"',
  '"\\/"',
  '"\u2028"',
  '"\u{1f600}"',
  '"\u007f"',
];

const KEYS = [
  '"a"',
  '"__proto__"',
  '"constructor"',
  '""',
  '"0"',
  '"1"',
  '"10"',
  '"01"',
  '"4294967295"',
  '"4294967294"',
  '"-1"',
  '"\\u0000"',
  '"\\ud800"',
  '"\\u00e9"',
  '"e\\u0301"',
];

/** Pieces a mutation inserts or substitutes. */
const TOKENS = [
  '"',
  '\\',
  '\\u',
  '\\ud800',
  '\\udc00',
  '\\uDBFF\\uDFFF',
  '\\u00',
  '\\x',
  '{',
  '}',
  '[',
  ']',
  ',',
  ':',
  '-',
  '+',
  '.',
  'e',
  'E',
  '0',
  '00',
  '9',
  'e999999999',
  'e-999999999',
  'true',
  'null',
  'nul',
  'NaN',
  'Infinity',
  '\ufeff',
  ' ',
  '\t',
  '\n',
  '\r',
  '\u00a0',
  '\u2028',
  '\u000b',
  '\u000c',
  '\u0000',
  '\u001f',
  '\u{1f600}',
  'é',
  '"__proto__":',
  '"a":1,"a":',
];

function value(r: Random, depth: number): string {
  if (depth > 3 || r.bool(0.4)) {
    return r.pick(ATOMS);
  }
  const n = r.int(4);
  if (r.bool()) {
    const items: string[] = [];
    for (let i = 0; i < n; i++) items.push(value(r, depth + 1));
    return '[' + items.join(',') + ']';
  }
  const entries: string[] = [];
  for (let i = 0; i < n; i++) {
    entries.push(r.pick(KEYS) + ':' + value(r, depth + 1));
  }
  return '{' + entries.join(',') + '}';
}

/** A string edited a few times: insertions, deletions, substitutions. */
function mutateText(r: Random, s: string): string {
  const edits = 1 + r.int(3);
  for (let e = 0; e < edits; e++) {
    const i = r.int(s.length + 1);
    switch (r.int(5)) {
      case 0:
        s = s.slice(0, i) + r.pick(TOKENS) + s.slice(i);
        break;
      case 1:
        s = s.slice(0, i) + s.slice(i + 1 + r.int(3));
        break;
      case 2:
        s = s.slice(0, i) + r.pick(TOKENS) + s.slice(i + 1);
        break;
      case 3: {
        const j = i + r.int(8);
        s = s.slice(0, j) + s.slice(i, j) + s.slice(j);
        break;
      }
      default:
        s = s.slice(0, i);
    }
  }
  return s;
}

/** Whether `s` has no lone surrogate (`String#isWellFormed`). */
function isWellFormed(s: string): boolean {
  return !/\p{Cs}/u.test(s);
}

type JSONCase = {input: string; ok: boolean; stringify?: string};

function jsonCase(input: string): JSONCase {
  let parsed: unknown;
  try {
    parsed = JSON.parse(input);
  } catch (e) {
    if (!(e instanceof SyntaxError)) throw e;
    return {input, ok: false};
  }
  return {input, ok: true, stringify: JSON.stringify(parsed)};
}

function jsonCorpus(r: Random, count: number): JSONCase[] {
  const cases: JSONCase[] = [];
  const seen = new Set<string>();
  const add = (input: string) => {
    // The vector file is UTF-8: a lone surrogate would not survive it.
    if (!isWellFormed(input) || seen.has(input)) return;
    seen.add(input);
    cases.push(jsonCase(input));
  };
  while (cases.length < count) {
    const base = value(r, 0);
    add(r.bool(0.25) ? base : mutateText(r, base));
  }
  return cases;
}

/** Nesting at the Swift parser's limit of 1,500, then mutated. */
function deepCases(r: Random): JSONCase[] {
  const cases: JSONCase[] = [];
  for (const depth of [1499, 1500]) {
    const arrays = '['.repeat(depth) + ']'.repeat(depth);
    const objects = '{"a":'.repeat(depth - 1) + '{}' + '}'.repeat(depth - 1);
    cases.push(jsonCase(arrays), jsonCase(objects));
    for (let i = 0; i < 4; i++) {
      // Edits near the middle, where the innermost value is.
      const m = arrays.length / 2;
      cases.push(
        jsonCase(
          arrays.slice(0, m) +
            mutateText(r, arrays.slice(m - 2, m + 2)) +
            arrays.slice(m),
        ),
      );
    }
  }
  return cases;
}

test('harden-json', () => {
  const r = new Random(JSON_SEED);
  emitVectors(
    'harden-json',
    'sweep-json',
    'Random and randomly edited JSON texts. `ok` is whether `JSON.parse` accepts the text; `stringify` is `JSON.stringify` of what it parsed.',
    [...deepCases(r), ...jsonCorpus(r, 1500)],
    undefined,
    {compact: true},
  );
  emitGeneratedVectors(
    'harden-json-fuzz',
    'sweep-json',
    'Random and randomly edited JSON texts; see vectors/harden-json.json.',
    jsonCorpus(new Random(JSON_FUZZ_SEED), 20000),
  );
});

type VectorCase = Record<string, unknown>;

function checkedInCases(name: string): VectorCase[] {
  const path = fileURLToPath(
    new URL(`../../vectors/${name}.json`, import.meta.url),
  );
  return (JSON.parse(readFileSync(path, 'utf8')) as {cases: VectorCase[]})
    .cases;
}

test('harden-downstream-text', () => {
  const r = new Random(DOWNSTREAM_SEED);
  const valid = checkedInCases('protocol-downstream')
    .filter(c => c['ok'] === true && 'message' in c)
    .map(c => JSON.stringify(c['message']));
  const cases: {text: string; ok: boolean}[] = [];
  for (let i = 0; i < 6000; i++) {
    const text = mutateText(r, r.pick(valid));
    if (!isWellFormed(text)) continue;
    let ok = false;
    try {
      ok = v.test(JSON.parse(text), downstreamSchema, 'passthrough').ok;
    } catch (e) {
      if (!(e instanceof SyntaxError)) throw e;
    }
    cases.push({text, ok});
  }
  emitGeneratedVectors(
    'harden-downstream-text',
    'harden-downstream-text',
    'Downstream messages edited as text. `ok` is whether `JSON.parse` and then `downstreamSchema` accept it.',
    cases,
  );
});

function hex(b: Uint8Array): string {
  return Buffer.from(b).toString('hex');
}

const INVALID_UTF8 = [
  [0xff],
  [0xc0, 0xaf],
  [0xed, 0xa0, 0x80],
  [0xf4, 0x90, 0x80, 0x80],
  [0xe4, 0xb8],
  [0x80],
  [0xef, 0xbb, 0xbf],
  [0x00],
];

function mutateBytes(r: Random, b: number[]): number[] {
  b = [...b];
  const edits = 1 + r.int(3);
  for (let e = 0; e < edits; e++) {
    const i = r.int(b.length + 1);
    switch (r.int(5)) {
      case 0:
        if (i < b.length) b[i] ^= 1 << r.int(8);
        break;
      case 1:
        if (i < b.length) b[i] = r.int(256);
        break;
      case 2:
        b.splice(i, 0, ...r.pick(INVALID_UTF8));
        break;
      case 3:
        b.splice(i, 1 + r.int(4));
        break;
      default:
        b.length = Math.min(b.length, i);
    }
  }
  return b;
}

function pokeChunksOK(messages: Uint8Array[]): boolean {
  const decoder = new TextDecoder('utf-8', {fatal: true});
  let text = '';
  try {
    for (const m of messages) {
      if (m[0] !== POKE_CHUNK_MESSAGE_TYPE) return false;
      text += decoder.decode(m.subarray(1), {stream: true});
    }
    text += decoder.decode();
    const parsed: unknown = JSON.parse(text);
    return v.test(parsed, pokePartsSchema, 'passthrough').ok;
  } catch {
    return false;
  }
}

test('harden-poke-bytes', () => {
  const r = new Random(POKE_SEED);
  const valid = checkedInCases('protocol-poke-chunks')
    .filter(c => c['ok'] === true)
    .map(c =>
      (c['messages'] as string[]).flatMap(m => [
        ...Buffer.from(m, 'hex').subarray(1),
      ]),
    )
    .filter(p => p.length > 2);
  const cases: {messages: string[]; ok: boolean}[] = [];
  for (let i = 0; i < 4000; i++) {
    const payload = mutateBytes(r, r.pick(valid));
    // Cut into up to three chunks, anywhere, even inside a UTF-8 sequence.
    const cuts = [r.int(payload.length + 1), r.int(payload.length + 1)].sort(
      (a, b) => a - b,
    );
    const pieces = [
      payload.slice(0, cuts[0]),
      payload.slice(cuts[0], cuts[1]),
      payload.slice(cuts[1]),
    ].filter((p, j) => j === 0 || p.length > 0);
    const messages = pieces.map(p => {
      const type = r.bool(0.02) ? r.int(256) : POKE_CHUNK_MESSAGE_TYPE;
      return Uint8Array.from([type, ...p]);
    });
    cases.push({messages: messages.map(hex), ok: pokeChunksOK(messages)});
  }
  emitGeneratedVectors(
    'harden-poke-bytes',
    'harden-poke-bytes',
    'Binary poke chunks of valid pokes edited byte by byte, each message in hex. `ok` is whether `TextDecoder` (fatal), `JSON.parse`, and `pokePartsSchema` accept them.',
    cases,
  );
});
