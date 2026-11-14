import {readFileSync} from 'node:fs';
import {test} from 'vitest';
import {astToZQL} from '../../../ast-to-zql/src/ast-to-zql.ts';
import type {AST, CorrelatedSubquery} from '../../../zero-protocol/src/ast.ts';
import {emitVectors} from '../vectors.ts';

function corpus(name: string): AST[] {
  const file = JSON.parse(
    readFileSync(
      new URL(`../../vectors/${name}.json`, import.meta.url),
      'utf8',
    ),
  ) as {cases: {ast: AST; normalized?: AST; valid?: boolean}[]};
  return file.cases.flatMap(c =>
    c.valid === false ? [] : [c.ast, ...(c.normalized ? [c.normalized] : [])],
  );
}

const sq = (
  alias: string,
  ast: Omit<AST, 'table'> & {table?: string},
): CorrelatedSubquery => ({
  correlation: {parentField: ['id'], childField: ['issueId']},
  subquery: {table: ast.table ?? 'comment', alias, ...ast},
});

/** Shapes the corpora lack: junction hops, NOT EXISTS, escapes. */
const extra: AST[] = [
  {
    table: 'issue',
    related: [
      {
        ...sq('labels', {
          table: 'issueLabel',
          related: [
            sq('labels', {
              table: 'label',
              orderBy: [['name', 'asc']],
              limit: 3,
            }),
          ],
        }),
        hidden: true,
      },
      sq('comments', {limit: 0}),
      sq('owner', {orderBy: []}),
    ],
  },
  {
    table: 'issue',
    where: {
      type: 'or',
      conditions: [
        {
          type: 'correlatedSubquery',
          op: 'NOT EXISTS',
          related: sq('zsubq_comments', {
            where: {
              type: 'simple',
              op: 'LIKE',
              left: {type: 'column', name: 'body'},
              right: {type: 'literal', value: 'it\'s \\ "quoted"\n'},
            },
          }),
        },
        {
          type: 'and',
          conditions: [
            {
              type: 'simple',
              op: '=',
              left: {type: 'column', name: 'ownerId'},
              right: {type: 'static', anchor: 'authData', field: 'sub'},
            },
            {
              type: 'simple',
              op: 'IN',
              left: {type: 'column', name: 'id'},
              right: {type: 'literal', value: ['a', 1, true]},
            },
            {
              type: 'correlatedSubquery',
              op: 'EXISTS',
              related: sq('zsubq_owner', {table: 'user'}),
              flip: true,
            },
          ],
        },
      ],
    },
    start: {row: {id: 'x', n: 1.5}, exclusive: false},
  },
  {
    table: 'issue',
    where: {
      type: 'correlatedSubquery',
      op: 'EXISTS',
      related: sq('zsubq_labels', {
        table: 'issueLabel',
        where: {
          type: 'correlatedSubquery',
          op: 'EXISTS',
          related: sq('zsubq_zhidden_labels', {
            table: 'label',
            where: {
              type: 'simple',
              op: '!=',
              left: {type: 'column', name: 'name'},
              right: {type: 'literal', value: null},
            },
          }),
        },
      }),
      scalar: true,
    },
    limit: 10,
    start: {row: {id: 'y'}, exclusive: true},
  },
  {
    table: 'issue',
    where: {
      type: 'and',
      conditions: [
        {
          type: 'simple',
          op: '>',
          left: {
            type: 'json',
            value: {type: 'column', name: 'meta'},
            path: ["a'b", 0],
          },
          right: {type: 'static', anchor: 'preMutationRow', field: ['x', 'y']},
        },
        {
          type: 'or',
          conditions: [
            {
              type: 'simple',
              op: 'IS',
              left: {type: 'column', name: 'closed'},
              right: {type: 'literal', value: false},
            },
          ],
        },
      ],
    },
  },
  {table: 'issue', where: {type: 'and', conditions: []}},
  {table: 'issue', where: {type: 'or', conditions: []}},
] as AST[];

test('ast-to-zql', () => {
  const asts = [...corpus('ast'), ...corpus('ast-features'), ...extra];
  const seen = new Set<string>();
  const cases = [];
  for (const ast of asts) {
    const key = JSON.stringify(ast);
    if (seen.has(key)) continue;
    seen.add(key);
    cases.push({
      ast,
      zql: astToZQL(ast),
      redacted: astToZQL(ast, {redactLiterals: true}),
    });
  }
  emitVectors(
    'ast-to-zql',
    'ast-to-zql',
    'astToZQL (ast-to-zql/src/ast-to-zql.ts) of the ASTs of vectors/ast.json and vectors/ast-features.json (each as given and normalized) and a few more: as is, and with redactLiterals.',
    cases,
    undefined,
    {compact: true},
  );
});
