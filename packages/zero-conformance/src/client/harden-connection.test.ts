import {test} from 'vitest';
import {emitVectors} from '../vectors.ts';
import {
  runScenario,
  START_TIME,
  type Scenario,
  type StepResult,
} from './harness.ts';
import {hardenConnectionScenarios} from './scenarios/harden-connection.ts';

type Case = Scenario & {expect: StepResult[]};

test('client hardening scenarios: connection', {timeout: 600_000}, async () => {
  const cases: Case[] = [];
  for (const scenario of hardenConnectionScenarios) {
    cases.push({...scenario, expect: await runScenario(scenario)});
  }
  emitVectors(
    'client-harden-connection',
    'client-scenarios',
    'Hardening scenarios for the connection and the sync protocol, run against the JS zero-client like vectors/client.json. See src/client/scenarios/harden-connection.ts.',
    cases,
    {startTime: START_TIME},
    {compact: true},
  );
});
