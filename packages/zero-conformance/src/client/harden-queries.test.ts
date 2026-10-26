// Hardening client scenarios for views and queries, kept apart from
// `client.json`. `swift/Tests/ConformanceTests/HardenQueryTests.swift`
// replays them.
import {test} from 'vitest';
import {emitVectors} from '../vectors.ts';
import {
  runScenario,
  START_TIME,
  type Scenario,
  type StepResult,
} from './harness.ts';
import {hardenQueryScenarios} from './scenarios/harden-queries.ts';

type Case = Scenario & {expect: StepResult[]};

test('harden query scenarios', {timeout: 600_000}, async () => {
  const cases: Case[] = [];
  for (const scenario of hardenQueryScenarios) {
    cases.push({...scenario, expect: await runScenario(scenario)});
  }
  emitVectors(
    'harden-queries',
    'client-scenarios',
    'Hardening client scenarios: re-entrant listeners, odd TTLs, query change throttling, pokes that leave views unchanged. See src/client/harness.ts.',
    cases,
    {startTime: START_TIME},
    {compact: true},
  );
});
