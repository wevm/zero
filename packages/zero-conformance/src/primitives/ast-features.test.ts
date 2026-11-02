/**
 * The AST features of sync protocols 54–57: aggregates (top level and per
 * relationship, with a JSON path or over distinct columns), `groupBy`, and
 * JSON path references in conditions. For each AST: whether the wire schema
 * a client's AST is checked with accepts it, and for those it accepts, its
 * normalized form, its hash, and its server names (or why mapping fails).
 */
import {test} from 'vitest';
import * as v from '../../../shared/src/valita.ts';
import {
  mapAST,
  normalizeAST,
  strictASTSchema,
  type AST,
  type Condition,
} from '../../../zero-protocol/src/ast.ts';
import {hashOfQueryInternals} from '../../../zero-protocol/src/query-hash-visitor.ts';
import {hashOfAST} from '../../../zero-protocol/src/query-hash.ts';
import {clientToServer} from '../../../zero-schema/src/name-mapper.ts';
import type {Format} from '../../../zero-types/src/format.ts';
import {schema} from '../../../zql/src/query/test/test-schemas.ts';
import {emitVectors} from '../vectors.ts';

const col = (name: string) => ({type: 'column', name}) as const;
const lit = (value: unknown) => ({type: 'literal', value}) as const;
const json = (name: string, ...path: (string | number)[]) =>
  ({type: 'json', value: col(name), path}) as const;
const cmp = (left: unknown, op: string, right: unknown) =>
  ({type: 'simple', op, left, right}) as unknown as Condition;

const comments = {
  correlation: {parentField: ['id'], childField: ['issueId']},
  subquery: {table: 'comment', alias: 'comments'},
} as const;

/** ASTs the wire schema accepts. */
const valid: unknown[] = [
  {table: 'issue', aggregate: {fn: 'count'}},
  {
    table: 'issue',
    where: cmp(col('closed'), '=', lit(false)),
    aggregate: {fn: 'sum', field: 'createdAt'},
  },
  {
    table: 'issue',
    orderBy: [['createdAt', 'desc']],
    limit: 10,
    aggregate: {fn: 'max', field: 'title'},
  },
  // The wire schema leaves a count with a field to the operator.
  {table: 'issue', aggregate: {fn: 'count', field: 'title'}},
  {table: 'issue', aggregate: {fn: 'min', field: 'ownerId'}},
  {table: 'user', aggregate: {fn: 'avg', field: 'metadata', path: ['points']}},
  {table: 'user', aggregate: {fn: 'min', field: 'metadata', path: ['a', 0]}},
  // An object key and an array index hash apart.
  {table: 'user', aggregate: {fn: 'max', field: 'metadata', path: ['0']}},
  {table: 'user', aggregate: {fn: 'max', field: 'metadata', path: [0]}},
  {
    table: 'user',
    aggregate: {fn: 'sum', field: 'metadata', path: [2 ** 31 - 1]},
  },
  {table: 'issue', groupBy: ['ownerId']},
  {table: 'issue', groupBy: ['closed', 'ownerId']},
  {
    table: 'issue',
    groupBy: ['ownerId', 'closed'],
    orderBy: [['closed', 'desc']],
    aggregate: {fn: 'count'},
  },
  {
    table: 'issue',
    where: cmp(col('closed'), '=', lit(false)),
    groupBy: ['ownerId'],
    aggregate: {fn: 'avg', field: 'createdAt'},
  },
  {table: 'issue', aggregate: {fn: 'count', distinct: ['ownerId']}},
  {table: 'issue', aggregate: {fn: 'count', distinct: ['ownerId', 'closed']}},
  {table: 'issue', related: [{...comments, aggregate: {fn: 'count'}}]},
  {
    table: 'issue',
    related: [
      {
        ...comments,
        subquery: {
          ...comments.subquery,
          where: cmp(col('authorId'), '=', lit('u1')),
        },
        aggregate: {fn: 'sum', field: 'createdAt'},
      },
    ],
  },
  // Sorted by alias; the aggregate rides along with its entry.
  {
    table: 'issue',
    related: [
      {
        correlation: {parentField: ['ownerId'], childField: ['id']},
        subquery: {table: 'user', alias: 'owner'},
      },
      {...comments, aggregate: {fn: 'max', field: 'text'}},
    ],
  },
  // A junction aggregate's field is a column of the destination: issueId is
  // issue_id on comment but issueId on issueLabel.
  {
    table: 'issue',
    related: [
      {
        correlation: {parentField: ['id'], childField: ['issueId']},
        subquery: {
          table: 'issueLabel',
          alias: 'labels',
          related: [
            {
              correlation: {parentField: ['labelId'], childField: ['id']},
              subquery: {table: 'comment', alias: 'labels'},
            },
          ],
        },
        aggregate: {fn: 'min', field: 'issueId'},
      },
    ],
  },
  {
    table: 'issue',
    related: [
      {
        ...comments,
        subquery: {
          ...comments.subquery,
          related: [
            {
              correlation: {parentField: ['authorId'], childField: ['id']},
              subquery: {table: 'user', alias: 'author'},
              aggregate: {fn: 'avg', field: 'metadata', path: ['n']},
            },
          ],
        },
      },
    ],
  },
  {table: 'user', where: cmp(json('metadata', 'registrar'), '=', lit('x'))},
  {
    table: 'user',
    where: cmp(json('metadata', 'altContacts', 0), 'LIKE', lit('a%')),
  },
  {
    table: 'user',
    where: cmp(json('metadata', 'n'), 'IN', lit([1, 2.5, -0])),
  },
  {table: 'user', where: cmp(json('metadata', 'n'), 'NOT IN', lit(null))},
  {table: 'user', where: cmp(json('metadata', 'n'), 'IN', lit([]))},
  {table: 'user', where: cmp(json('metadata', 'n'), 'IS', lit(null))},
  {
    table: 'user',
    where: cmp(json('metadata', 'login'), '=', {
      type: 'static',
      anchor: 'authData',
      field: 'sub',
    }),
  },
  // Sorted with the other conditions: columns, then json, then literals.
  {
    table: 'user',
    where: {
      type: 'and',
      conditions: [
        cmp(lit(1), '=', lit(1)),
        cmp(json('metadata', 'b'), '=', lit(1)),
        cmp(json('metadata', 'a', 1), '=', lit(1)),
        cmp(json('metadata', 'a', '1'), '=', lit(1)),
        cmp(json('metadata', 'a', 0), '=', lit(1)),
        cmp(json('id', 'a'), '=', lit(1)),
        cmp(col('name'), '=', lit('n')),
      ],
    },
  },
  // Not json columns: the wire accepts them, mapping to server names fails.
  {table: 'issue', where: cmp(json('title', 'a'), '=', lit('x'))},
  {table: 'issue', aggregate: {fn: 'sum', field: 'title', path: ['a']}},
  {table: 'issue', aggregate: {fn: 'sum', field: 'nope'}},
  {table: 'issue', groupBy: ['nope']},
  {table: 'issue', aggregate: {fn: 'count', distinct: ['nope']}},
];

/** ASTs the wire schema rejects. */
const invalid: unknown[] = [
  {table: 'issue', aggregate: {fn: 'median'}},
  {table: 'issue', aggregate: {}},
  {table: 'issue', aggregate: {fn: 'count', extra: 1}},
  {table: 'issue', aggregate: {fn: 'sum', field: 1}},
  {table: 'issue', aggregate: {fn: 'sum', field: null}},
  {table: 'user', aggregate: {fn: 'sum', field: 'metadata', path: []}},
  {table: 'user', aggregate: {fn: 'sum', field: 'metadata', path: [-1]}},
  {table: 'user', aggregate: {fn: 'sum', field: 'metadata', path: [1.5]}},
  {table: 'user', aggregate: {fn: 'sum', field: 'metadata', path: [2 ** 31]}},
  {table: 'user', aggregate: {fn: 'sum', field: 'metadata', path: [true]}},
  {table: 'user', aggregate: {fn: 'sum', path: ['a']}},
  {table: 'user', aggregate: {fn: 'count', field: 'metadata', path: ['a']}},
  {table: 'issue', aggregate: {fn: 'count', distinct: []}},
  {table: 'issue', aggregate: {fn: 'sum', distinct: ['ownerId']}},
  {table: 'issue', aggregate: {fn: 'count', field: 'id', distinct: ['id']}},
  {table: 'issue', aggregate: {fn: 'count', distinct: [1]}},
  {table: 'issue', groupBy: []},
  {table: 'issue', groupBy: 'ownerId'},
  {table: 'issue', groupBy: [null]},
  {table: 'issue', related: [{...comments, aggregate: {fn: 'mode'}}]},
  {
    table: 'issue',
    related: [{...comments, aggregate: {fn: 'count', distinct: []}}],
  },
  {table: 'user', where: cmp(json('metadata'), '=', lit(1))},
  {table: 'user', where: cmp(json('metadata', -1), '=', lit(1))},
  {table: 'user', where: cmp(json('metadata', 0.5), '=', lit(1))},
  {
    table: 'user',
    where: cmp(json('metadata', null as unknown as string), '=', lit(1)),
  },
  {table: 'user', where: cmp(col('id'), '=', json('metadata', 'a'))},
  {table: 'user', where: cmp(json('metadata', 'a'), 'IN', lit('x'))},
  {table: 'user', where: cmp(json('metadata', 'a'), 'NOT IN', lit(1))},
  {table: 'user', where: cmp(json('metadata', 'a'), '=', lit(['x']))},
  {table: 'user', where: cmp(json('metadata', 'a'), 'IN', lit(['x', 1]))},
  {
    table: 'user',
    where: cmp(json('metadata', 'a'), 'NOT IN', lit([true, 'true'])),
  },
  {
    table: 'user',
    where: cmp(
      {type: 'json', value: lit('metadata'), path: ['a']},
      '=',
      lit(1),
    ),
  },
  {
    table: 'user',
    where: cmp(
      {type: 'json', value: col('metadata'), path: ['a'], extra: 1},
      '=',
      lit(1),
    ),
  },
  {
    table: 'user',
    where: cmp(
      {type: 'json', value: {...col('metadata'), extra: 1}, path: ['a']},
      '=',
      lit(1),
    ),
  },
  {table: 'user', where: cmp({type: 'json', path: ['a']}, '=', lit(1))},
];

const mapper = clientToServer(schema.tables);

function astCase(json: unknown) {
  let parsed: AST;
  try {
    parsed = v.parse(json, strictASTSchema);
  } catch (e) {
    return {ast: json, valid: false, error: (e as Error).message};
  }
  let mapped: unknown;
  try {
    mapped = {mapped: JSON.parse(JSON.stringify(mapAST(parsed, mapper)))};
  } catch (e) {
    mapped = {mapError: (e as Error).message};
  }
  return {
    ast: json,
    valid: true,
    normalized: JSON.parse(JSON.stringify(normalizeAST(parsed))),
    hash: hashOfAST(parsed),
    ...(mapped as object),
  };
}

test('ast-features', () => {
  for (const ast of valid) {
    if (!astCase(ast).valid) {
      throw new Error(`expected a valid AST: ${JSON.stringify(ast)}`);
    }
  }
  for (const ast of invalid) {
    if (astCase(ast).valid) {
      throw new Error(`expected an invalid AST: ${JSON.stringify(ast)}`);
    }
  }
  emitVectors(
    'ast-features',
    'ast-features',
    'ASTs with aggregates, groupBy and JSON paths: whether strictASTSchema accepts each, and for those it accepts normalizeAST(ast) (as JSON), hashOfAST(ast), and mapAST(ast) to the zql test schema server names or its error.',
    [...valid, ...invalid].map(astCase),
  );
});

const formats: Format[] = [
  {singular: true, relationships: {}, aggregate: {fn: 'count'}},
  {
    singular: true,
    relationships: {},
    aggregate: {fn: 'count', field: undefined},
  },
  {singular: true, relationships: {}, aggregate: {fn: 'sum', field: 'n'}},
  {
    singular: true,
    relationships: {},
    aggregate: {fn: 'min', field: 'metadata', path: ['a', 0]},
  },
  {
    singular: true,
    relationships: {},
    aggregate: {fn: 'count', distinct: ['ownerId']} as Format['aggregate'],
  },
  {
    singular: false,
    relationships: {
      comments: {singular: true, relationships: {}, aggregate: {fn: 'count'}},
    },
  },
];

test('query-internals-hash', () => {
  const ast: AST = {table: 'issue', aggregate: {fn: 'count'}};
  emitVectors(
    'query-internals-hash',
    'query-internals-hash',
    'hashOfQueryInternals(ast, format, "client", undefined, undefined, undefined) for formats with aggregates.',
    formats.map(format => ({
      ast,
      format: JSON.parse(JSON.stringify(format)),
      hash: hashOfQueryInternals(
        ast,
        format,
        'client',
        undefined,
        undefined,
        undefined,
      ),
    })),
  );
});
