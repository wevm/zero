import {mergeConfig} from 'vitest/config';
import config from '../shared/src/tool/vitest-config.ts';

// Vectors are written to disk, so the generators run in Node rather than in
// the browser like most zero-client tests.
export default mergeConfig(config, {
  test: {
    name: 'zero-conformance',
    browser: {
      enabled: false,
    },
    // A materialize that throws in JS has already registered its query, and
    // that query's completion promise, which nothing holds, rejects when the
    // client closes (`materializeImpl` in zql). The scenarios that check a
    // failed materialize (harden-queries) leave one behind; that rejection
    // isn't part of what they record.
    onUnhandledError: error =>
      !(
        'error' in error &&
        error.error === 'app' &&
        'details' in error &&
        (error.details as {kind?: unknown} | undefined)?.kind === 'ClientClosed'
      ),
  },
});
