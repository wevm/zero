import {test} from 'vitest';
import {
  aggregateRowKey,
  aggregateRowKeyColumns,
  toPrimaryKeyString,
} from '../../../zero-client/src/client/keys.ts';
import type {CompoundKey} from '../../../zero-protocol/src/ast.ts';
import type {Row} from '../../../zero-protocol/src/data.ts';
import {interestingStrings, Random} from '../random.ts';
import {emitVectors} from '../vectors.ts';

test('primary-key-string', () => {
  const r = new Random(0x6b6579);
  const cases: {table: string; primaryKey: CompoundKey; row: Row}[] = [];
  const scalars = [
    ...interestingStrings,
    0,
    -0,
    1,
    -1,
    1.5,
    1e21,
    1e-7,
    2 ** 53,
    123456789,
    true,
    false,
  ];
  for (const v of scalars) {
    cases.push({table: 'issue', primaryKey: ['id'], row: {id: v, other: 'x'}});
  }
  for (let i = 0; i < 100; i++) {
    cases.push({
      table: r.pick(['issueLabel', 'a/b', 'T']),
      primaryKey: ['a', 'b'],
      row: {a: r.pick(scalars), b: r.pick(scalars), c: 1},
    });
  }
  cases.push({
    table: 'three',
    primaryKey: ['z', 'a', 'm'],
    row: {a: 1, m: 'x', z: true},
  });
  emitVectors(
    'primary-key-string',
    'primary-key-string',
    'toPrimaryKeyString(table, primaryKey, row): the `e/` storage key of a row.',
    cases.map(c => ({
      ...c,
      key: toPrimaryKeyString(c.table, c.primaryKey, c.row),
    })),
  );
});

test('aggregate-row-key', () => {
  const r = new Random(0x616767);
  const values = [...interestingStrings, 0, -0, 1, 1.5, 2 ** 53, true, false];
  const payload = () =>
    r.pick([
      {_0_value: r.pick([0, 3, null])},
      {_0_value: 1.5, _0_sum: 3, _0_count: 2, _0_rows: 2},
      {_0_value: null, _0_sum: 0, _0_count: 0},
    ]);
  const cases: {table: string; row: Row}[] = [];
  for (let i = 0; i < 60; i++) {
    // A top-level aggregate's and a grouped query's rows, keyed by `_0_key`
    // whatever else they carry.
    cases.push({
      table: r.pick(['aggregate:q1', 'aggregate:q1:#group', 'aggregate:#']),
      row: {
        _0_key: r.pick(values),
        ...(r.pick([true, false]) ? {closed: r.pick(values)} : {}),
        ...(r.pick([true, false]) ? {_0_version: '01'} : {}),
        ...payload(),
      },
    });
    // A relationship aggregate's, keyed by its correlation columns, sorted,
    // even one named like the synthetic key; never by the row version.
    cases.push({
      table: r.pick(['aggregate:q1:comments', 'aggregate:q2:labels']),
      row: {
        ...(r.pick([true, false])
          ? {issueId: r.pick(values)}
          : {b: r.pick(values), a: r.pick(values)}),
        ...(r.pick([true, false]) ? {_0_key: r.pick(values)} : {}),
        ...(r.pick([true, false]) ? {_0_version: '02'} : {}),
        ...payload(),
      },
    });
  }
  // A key value that isn't a string, number or boolean.
  cases.push({table: 'aggregate:q1', row: {_0_key: null, _0_value: 1}});
  cases.push({
    table: 'aggregate:q1:comments',
    row: {issueId: null, _0_value: 1},
  });
  emitVectors(
    'aggregate-row-key',
    'aggregate-row-key',
    'aggregateRowKeyColumns(table, row) and aggregateRowKey(table, row): the `a/` storage key of a synced aggregate row; `throws` when a key value is invalid.',
    cases.map(c => {
      let key: string | undefined;
      try {
        key = aggregateRowKey(c.table, c.row);
      } catch {
        key = undefined;
      }
      return {
        ...c,
        columns: aggregateRowKeyColumns(c.table, c.row),
        ...(key === undefined ? {throws: true} : {key}),
      };
    }),
  );
});
