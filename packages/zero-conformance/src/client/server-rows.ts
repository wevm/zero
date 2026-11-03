/**
 * The rows zero-cache syncs for a named query, computed by JS's own
 * pipeline rather than written by hand: the query's AST in server names,
 * built in compute mode over server-named tables with the query ID the
 * client registers it under, and its output walked as the view-syncer's
 * streamer walks it (`streamNodes` in zero-cache's `pipeline-driver.ts`).
 * An aggregate's rows go to its synthetic table, stamped with a version.
 */
import type {ReadonlyJSONValue} from '../../../shared/src/json.ts';
import {mapAST} from '../../../zero-protocol/src/ast.ts';
import type {Row} from '../../../zero-protocol/src/data.ts';
import {hashOfNameAndArgs} from '../../../zero-protocol/src/query-hash.ts';
import {clientToServer} from '../../../zero-schema/src/name-mapper.ts';
import {buildPipeline} from '../../../zql/src/builder/builder.ts';
import {TestBuilderDelegate} from '../../../zql/src/builder/test-builder-delegate.ts';
import type {Node} from '../../../zql/src/ivm/data.ts';
import {MemorySource} from '../../../zql/src/ivm/memory-source.ts';
import type {SourceSchema} from '../../../zql/src/ivm/schema.ts';
import {makeSourceChangeAdd} from '../../../zql/src/ivm/source.ts';
import {consume} from '../../../zql/src/ivm/stream.ts';
import {asQueryInternals} from '../../../zql/src/query/query-internals.ts';
import {addContextToQuery} from '../../../zql/src/query/query-registry.ts';
import {queries, schema} from './app.ts';

type JSONObject = {readonly [key: string]: ReadonlyJSONValue};

/** Server-named rows by server table name. */
export type ServerData = Record<string, JSONObject[]>;

export type SyncedRow = {table: string; id: JSONObject; value: JSONObject};

function lookup(name: string) {
  let v: unknown = queries;
  for (const part of name.split('.')) {
    v = (v as Record<string, unknown>)[part];
  }
  return v as (...args: unknown[]) => unknown;
}

/** The rows the server syncs for `query(args)` over `data`, by table and key. */
export function syncedRows(
  query: string,
  args: ReadonlyJSONValue | undefined,
  data: ServerData,
  version: string,
): Map<string, SyncedRow> {
  const argList = args === undefined ? [] : [args];
  const request = lookup(query)(...argList);
  // oxlint-disable-next-line @typescript-eslint/no-explicit-any
  const q = addContextToQuery(request as any, undefined);
  const mapper = clientToServer(schema.tables);
  const ast = mapAST(asQueryInternals(q).ast, mapper);
  const sources: Record<string, MemorySource> = {};
  for (const [name, table] of Object.entries(schema.tables)) {
    const serverName = mapper.tableName(name);
    const columns = Object.fromEntries(
      Object.entries(table.columns).map(([c, v]) => [
        mapper.columnName(name, c),
        v,
      ]),
    );
    const source = new MemorySource(
      serverName,
      columns,
      table.primaryKey.map(c => mapper.columnName(name, c)) as [
        string,
        ...string[],
      ],
    );
    for (const row of data[serverName] ?? []) {
      consume(source.push(makeSourceChangeAdd(row as Row)));
    }
    sources[serverName] = source;
  }
  const input = buildPipeline(
    ast,
    new TestBuilderDelegate(sources),
    hashOfNameAndArgs(query, argList),
  );
  const rows = new Map<string, SyncedRow>();
  const walk = (schema: SourceSchema, nodes: Iterable<Node | 'yield'>) => {
    if (schema.system === 'permissions') {
      return;
    }
    for (const node of nodes) {
      if (node === 'yield') {
        continue;
      }
      const id = Object.fromEntries(
        schema.primaryKey.map(c => [c, node.row[c] as ReadonlyJSONValue]),
      );
      const value = (
        schema.isAggregate ? {...node.row, _0_version: version} : node.row
      ) as JSONObject;
      rows.set(JSON.stringify([schema.tableName, id]), {
        table: schema.tableName,
        id,
        value,
      });
      for (const [name, stream] of Object.entries(node.relationships)) {
        walk(schema.relationships[name], stream());
      }
    }
  };
  walk(input.getSchema(), input.fetch({}));
  input.destroy();
  return rows;
}

/**
 * The `rowsPatch` that takes the client from what the server synced for
 * `query(args)` over `before` (nothing when undefined) to what it syncs
 * over `after`: a put for each new or changed row, a del for each gone.
 */
export function serverRowsPatch(
  query: string,
  args: ReadonlyJSONValue | undefined,
  before: ServerData | undefined,
  after: ServerData,
  version = '01',
): ReadonlyJSONValue[] {
  const old = before
    ? syncedRows(query, args, before, version)
    : new Map<string, SyncedRow>();
  const next = syncedRows(query, args, after, version);
  const patch: ReadonlyJSONValue[] = [];
  for (const [key, row] of old) {
    if (!next.has(key)) {
      patch.push({op: 'del', tableName: row.table, id: row.id});
    }
  }
  for (const [key, row] of next) {
    const prev = old.get(key);
    const same =
      prev !== undefined &&
      JSON.stringify({...prev.value, _0_version: null}) ===
        JSON.stringify({...row.value, _0_version: null});
    if (!same) {
      patch.push({op: 'put', tableName: row.table, value: row.value});
    }
  }
  return patch;
}
