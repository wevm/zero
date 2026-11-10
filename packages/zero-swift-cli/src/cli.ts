/**
 * `zero-swift codegen` and `zero-swift drift record`. The generator is the
 * repository's shared packages/zerogen, which the build bundles in.
 */
import {createSwiftCli} from '../../zerogen/src/cli.ts';
import pkg from '../package.json' with {type: 'json'};

export {codegenFile, legacyArgv} from '../../zerogen/src/cli.ts';
export const cli = createSwiftCli(pkg.version);
