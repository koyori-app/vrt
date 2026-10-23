import assert from 'node:assert/strict';
import test from 'node:test';

import { installStorybookWorld } from './play-trace-storybook.harness.mjs';

test('real Storybook chained expect remains passing and retains CallRef paths', async () => {
  const channel = await installStorybookWorld('real-expect--passes');

  const { expect } = await import('storybook/test');
  // The instrumenter attaches to the ready addon channel in a promise callback.
  await new Promise((resolve) => setTimeout(resolve, 0));

  await expect('visible').toBe('visible');
  await new Promise((resolve) => setTimeout(resolve, 20));

  const rawCalls = channel.emitted
    .filter(([event]) => event === 'storybook/instrumenter/call')
    .map(([, payload]) => payload);
  const rawCallRef = rawCalls
    .flatMap((call) => call.path)
    .find((part) => part && typeof part === 'object' && typeof part.__callId__ === 'string');
  assert.ok(
    rawCallRef,
    `the real Storybook expect chain must emit an object CallRef path: ${JSON.stringify(rawCalls)}`,
  );

  const trace = window.__VRT_READY__.playTrace();
  assert.equal(trace.issue, null, `passing expect produced a trace issue: ${trace.issue}`);
  assert.ok(trace.calls.some((call) => call.status === 'done'));
  assert.ok(
    trace.calls.some((call) =>
      call.path.some(
        (part) => part && typeof part === 'object' && part.__callId__ === rawCallRef.__callId__,
      ),
    ),
    'the collector must retain the real Storybook CallRef path',
  );
});
