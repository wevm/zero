/**
 * `zero-rust codegen`. The generator is the repository's shared
 * packages/zerogen.
 */
import {createRustCli} from '../../zerogen/src/rust-cli.ts';
import pkg from '../package.json' with {type: 'json'};

export {codegenRustFile} from '../../zerogen/src/rust-cli.ts';
export const cli = createRustCli(pkg.version);
