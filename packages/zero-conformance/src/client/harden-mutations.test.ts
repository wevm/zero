import {test} from 'vitest';
import {emitVectors} from '../vectors.ts';
import {
  runScenario,
  START_TIME,
  type Scenario,
  type StepResult,
} from './harness.ts';
import {hardenMutationScenarios} from './scenarios/harden-mutations.ts';

type Case = Scenario & {expect: StepResult[]};

test(
  'client hardening scenarios for mutations',
  {timeout: 600_000},
  async () => {
    const cases: Case[] = [];
    for (const scenario of hardenMutationScenarios) {
      cases.push({...scenario, expect: await runScenario(scenario)});
    }
    emitVectors(
      'client-harden-mutations',
      'client-scenarios',
      'Hardening scenarios for mutations, run against the JS zero-client like client.json (see src/client/harness.ts and src/client/scenarios/harden-mutations.ts).',
      cases,
      {startTime: START_TIME},
      {compact: true},
    );
  },
);
