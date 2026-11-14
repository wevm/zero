import {test} from 'vitest';
import {emitVectors} from '../vectors.ts';
import {
  runScenario,
  START_TIME,
  type Scenario,
  type StepResult,
} from './harness.ts';
import {inspectorScenarios} from './scenarios/inspector.ts';

type Case = Scenario & {expect: StepResult[]};

test('client inspector scenarios', {timeout: 600_000}, async () => {
  const cases: Case[] = [];
  for (const scenario of inspectorScenarios) {
    cases.push({...scenario, expect: await runScenario(scenario)});
  }
  emitVectors(
    'client-inspector',
    'client-scenarios',
    'Inspector scenarios run against the JS zero-client like vectors/client.json (see src/client/harness.ts): the inspect step, the RPCs it sends and how their answers resolve, the query metrics, and the slow materialization logs.',
    cases,
    {startTime: START_TIME},
    {compact: true},
  );
});
