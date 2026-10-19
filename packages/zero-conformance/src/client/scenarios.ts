import type {Scenario} from './harness.ts';
import {connectionScenarios} from './scenarios/connection.ts';
import {errorScenarios} from './scenarios/errors.ts';
import {mutationScenarios} from './scenarios/mutations.ts';
import {pokeScenarios} from './scenarios/pokes.ts';
import {queryScenarios} from './scenarios/queries.ts';

export const scenarios: Scenario[] = [
  ...connectionScenarios,
  ...errorScenarios,
  ...pokeScenarios,
  ...queryScenarios,
  ...mutationScenarios,
];
