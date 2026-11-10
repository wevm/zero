import {fileURLToPath} from 'node:url';
import {mergeConfig} from 'vitest/config';
import config from '../shared/src/tool/vitest-config.ts';

export default mergeConfig(config, {
  resolve: {
    alias: [
      {
        find: /^@rocicorp\/zero$/,
        replacement: fileURLToPath(
          new URL('../zero/src/zero.ts', import.meta.url),
        ),
      },
    ],
  },
  test: {name: 'zerogen', browser: {enabled: false}},
});
