import {createSwiftCli} from 'zerogen/compat';
import pkg from '../package.json' with {type: 'json'};

export {codegenFile, legacyArgv} from 'zerogen/compat';
export const cli = createSwiftCli(pkg.version);
