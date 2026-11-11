/**
 * The app the Swift and Rust end-to-end tests talk to: named queries and mutators over
 * the zql test schema. The Swift tests define the same names
 * (`swift/Tests/E2ETests/`), as an app's Swift client would.
 */
import type {ReadonlyJSONValue} from '../../../shared/src/json.ts';
import {defineMutators} from '../../../zql/src/mutate/mutator-registry.ts';
import {defineMutator} from '../../../zql/src/mutate/mutator.ts';
import {createBuilder} from '../../../zql/src/query/create-builder.ts';
import {
  defineQueries,
  defineQuery,
} from '../../../zql/src/query/query-registry.ts';
import {schema} from '../../../zql/src/query/test/test-schemas.ts';

export {schema};

const zql = createBuilder(schema);

export const queries = defineQueries({
  issues: {
    open: defineQuery(() =>
      zql.issue
        .where('closed', false)
        .related('comments')
        .related('owner')
        .orderBy('createdAt', 'asc'),
    ),
    byID: defineQuery(({args}: {args: string}) =>
      zql.issue.where('id', args).related('comments').one(),
    ),
  },
  // Aggregates the server computes. The tests create issues but no
  // comments, so the comment counts stay the fixture's; the `titled` ones
  // see only the issues a test creates with its own title.
  aggregates: {
    commentCount: defineQuery(() => zql.comment.count()),
    commentCounts: defineQuery(() =>
      zql.issue
        .where('id', 'IN', ['i1', 'i2', 'i3', 'i4'])
        .related('comments', q => q.count())
        .orderBy('id', 'asc'),
    ),
    commentsByAuthor: defineQuery(() =>
      zql.comment.groupBy('authorId').count(),
    ),
    titled: defineQuery(({args}: {args: string}) =>
      zql.issue.where('title', args).count(),
    ),
    titledOwners: defineQuery(({args}: {args: string}) =>
      zql.issue.where('title', args).groupBy('ownerId').count(),
    ),
  },
});

type CreateIssue = {id: string; title: string; ownerId?: string | undefined};

export const mutators = defineMutators({
  issue: {
    create: defineMutator(
      async ({tx, args}: {tx: AnyTx; args: CreateIssue}) => {
        await tx.mutate.issue.insert({
          id: args.id,
          title: args.title,
          description: '',
          closed: false,
          ownerId: args.ownerId ?? null,
          createdAt: 100,
        });
      },
    ),
    close: defineMutator(async ({tx, args}: {tx: AnyTx; args: string}) => {
      await tx.mutate.issue.update({id: args, closed: true});
    }),
  },
});

// The mutators run against the schema's CRUD API without a typed transaction.
// oxlint-disable-next-line @typescript-eslint/no-explicit-any
type AnyTx = any;

export type Args = ReadonlyJSONValue | undefined;
