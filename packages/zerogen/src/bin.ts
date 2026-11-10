#!/usr/bin/env node
import pkg from '../package.json' with {type: 'json'};
import {createCli} from './cli.ts';

await createCli('zerogen', pkg.version).serve();
