import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const execFileAsync = promisify(execFile);
const repositoryRoot = fileURLToPath(new URL('..', import.meta.url));

class TestChannel {
  #listeners = new Map();

  emitted = [];

  on(event, listener) {
    const listeners = this.#listeners.get(event) ?? [];
    listeners.push(listener);
    this.#listeners.set(event, listeners);
    return this;
  }

  off(event, listener) {
    const listeners = this.#listeners.get(event) ?? [];
    this.#listeners.set(
      event,
      listeners.filter((candidate) => candidate !== listener),
    );
    return this;
  }

  once(event, listener) {
    const wrapped = (...args) => {
      this.off(event, wrapped);
      listener(...args);
    };
    return this.on(event, wrapped);
  }

  emit(event, ...args) {
    this.emitted.push([event, ...structuredClone(args)]);
    for (const listener of this.#listeners.get(event) ?? []) listener(...args);
    return this;
  }
}

async function readyHookScript() {
  const revision = process.env.PLAY_TRACE_GIT_REVISION;
  const source = revision
    ? (
        await execFileAsync(
          'git',
          ['show', `${revision}:apps/backend/crates/service/src/render/browser.rs`],
          { cwd: repositoryRoot, maxBuffer: 10 * 1024 * 1024 },
        )
      ).stdout
    : await readFile(
        new URL('../apps/backend/crates/service/src/render/browser.rs', import.meta.url),
        'utf8',
      );
  if (revision) {
    assert.match(source, /payload\.path\.every/, `expected the pre-fix collector at ${revision}`);
  }
  const match = source.match(/const READY_HOOK_SCRIPT: &str = r#"([\s\S]*?)"#;/);
  assert.ok(match, 'READY_HOOK_SCRIPT must remain extractable for the integration test');
  return match[1];
}

test('real Storybook chained expect remains passing and retains CallRef paths', async () => {
  const channel = new TestChannel();
  const documentElement = {};
  globalThis.document = { documentElement };
  const preview = {
    selectionStore: { selection: { storyId: 'real-expect--passes' } },
  };
  globalThis.window = {
    parent: {},
    location: { search: '' },
    document: globalThis.document,
    HTMLElement: class HTMLElement {},
    __STORYBOOK_ADDONS_CHANNEL__: channel,
    __STORYBOOK_PREVIEW__: preview,
  };
  window.window = window;
  globalThis.HTMLElement = window.HTMLElement;
  globalThis.__STORYBOOK_PREVIEW__ = preview;
  const addonStore = {
    getChannel: () => channel,
    ready: async () => channel,
  };
  globalThis.__STORYBOOK_ADDONS_PREVIEW = addonStore;
  window.__STORYBOOK_ADDONS_PREVIEW = addonStore;

  // Run the exact collector injected by the Rust renderer, not a test copy.
  Function(await readyHookScript())();

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
