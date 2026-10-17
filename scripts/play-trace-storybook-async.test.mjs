import assert from 'node:assert/strict';
import test from 'node:test';

import { installStorybookWorld } from './play-trace-storybook.harness.mjs';

test('real Storybook async call reports active then done under one id without a trace issue', async () => {
  const channel = await installStorybookWorld('real-async--passes');
  const { instrument } = await import('storybook/internal/instrumenter');
  // Like userEvent, find* and waitFor*: an intercepted call that returns a Promise.
  // The instrumenter emits the same call id twice, first `active` and then `done`.
  const { settle } = instrument(
    {
      settle: async (value) => {
        await new Promise((resolve) => setTimeout(resolve, 5));
        return value;
      },
    },
    { intercept: true },
  );
  // The instrumenter attaches to the ready addon channel in a promise callback.
  await new Promise((resolve) => setTimeout(resolve, 0));

  assert.equal(await settle('ok'), 'ok');
  await new Promise((resolve) => setTimeout(resolve, 20));

  const rawCalls = channel.emitted
    .filter(([event]) => event === 'storybook/instrumenter/call')
    .map(([, payload]) => payload);
  const settleCalls = rawCalls.filter((call) => call.method === 'settle');
  assert.equal(settleCalls.length, 2, `expected active and done CALLs: ${JSON.stringify(rawCalls)}`);
  assert.equal(settleCalls[0].id, settleCalls[1].id);
  assert.deepEqual(
    settleCalls.map((call) => call.status),
    ['active', 'done'],
  );

  const trace = window.__VRT_READY__.playTrace();
  assert.equal(trace.issue, null, `a passing async call produced a trace issue: ${trace.issue}`);
  assert.equal(trace.truncated, false);
  const settled = trace.calls.filter((call) => call.method === 'settle');
  assert.equal(settled.length, 1);
  assert.equal(settled[0].id, settleCalls[0].id);
  assert.equal(settled[0].status, 'done');
});
