#!/usr/bin/env node
import {cli} from './cli.ts';

await cli.serve(process.argv.slice(2));
