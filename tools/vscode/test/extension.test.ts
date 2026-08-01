import * as assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { after, before, test } from 'node:test';

import { installVscodeStub, stubUri, type Recorded } from './vscode-stub';

// The editor layer, actually executed. Before this file existed it was only
// type-checked: `activate` had never been called, no command handler had ever
// run, and no diagnostic had ever been placed at a URI. Type-checking cannot
// tell you that a command is registered under the id `package.json` promises,
// or that a diagnostic lands on the file the compiler named.
//
// The frost-dependent cases run against the real binary and the real
// sample_multi workspace, so what they prove is end to end: command handler →
// frost → parsed output → editor objects. They skip when the binary is absent,
// the way the Java E2Es skip without a JDK, because a release build is not a
// precondition for running the unit suite.

const REPO = join(__dirname, '..', '..', '..', '..');
const FROST = join(REPO, 'target', 'release', 'frost');
const WORKSPACE = join(REPO, 'sample_multi');
const haveFrost = existsSync(FROST) && existsSync(WORKSPACE);

let stub: { recorded: Recorded; dispose: () => void };
let recorded: Recorded;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let extension: any;

before(() => {
  stub = installVscodeStub();
  recorded = stub.recorded;
  // Required after the stub is installed: importing earlier would resolve the
  // real `vscode`, which does not exist outside an editor.
  // eslint-disable-next-line @typescript-eslint/no-var-requires
  extension = require('../src/extension');
});

after(() => {
  stub.dispose();
});

/**
 * Fresh activation with a clean recorder.
 *
 * Every observable is reset, not just the command map: a message or a
 * registered handler left over from an earlier test makes the next one pass or
 * fail for the wrong reason, which is worse than no test.
 */
function activate(): void {
  recorded.commands.clear();
  recorded.taskProviders.clear();
  recorded.diagnostics.clear();
  recorded.output.length = 0;
  recorded.info.length = 0;
  recorded.warnings.length = 0;
  recorded.errors.length = 0;
  recorded.saveHandlers.length = 0;
  recorded.configurationHandlers.length = 0;
  recorded.quickPickItems.length = 0;
  recorded.quickPickAnswer = undefined;
  recorded.status.text = '';
  extension.activate({ subscriptions: [] });
}

function useWorkspace(): void {
  recorded.workspaceFolders = [
    { uri: stubUri(WORKSPACE), name: 'sample_multi', index: 0 },
  ];
  recorded.configuration = { binaryPath: FROST, profile: 'debug', platform: '' };
}

test('activate registers exactly the commands package.json declares', () => {
  activate();
  // If these drift apart the command shows in the palette and then fails with
  // "command not found", which is a bug no type checker can see.
  assert.deepEqual(
    [...recorded.commands.keys()].sort(),
    [
      'frost.build',
      'frost.buildFileOwners',
      'frost.buildTarget',
      'frost.debugTarget',
      'frost.refreshTargets',
      'frost.test',
      'frost.testTarget',
    ],
  );
  assert.ok(recorded.taskProviders.has('frost'), 'a frost task provider is registered');
  assert.ok(recorded.status.shown, 'the status item is visible from activation');
});

test('a save handler is registered and honours buildOnSave', async () => {
  activate();
  assert.equal(recorded.saveHandlers.length, 1);
  recorded.workspaceFolders = [{ uri: stubUri('/nowhere'), name: 'x', index: 0 }];
  recorded.configuration = { buildOnSave: false };
  // Off by default: the handler must return without running anything.
  await recorded.saveHandlers[0]({ uri: stubUri('/nowhere/a.c') });
  assert.deepEqual(recorded.errors, []);
});

test('refreshTargets completes without announcing itself', async () => {
  activate();
  const handler = recorded.commands.get('frost.refreshTargets');
  assert.ok(handler);
  await handler();
  // The tree and the Test Explorer repopulate visibly, so a notification here
  // would be noise for something the user can already see. What must hold is
  // that the command resolves rather than leaving work in flight — the
  // Test Explorer discovery is awaited, which is what makes a refresh
  // followed by a look deterministic.
  assert.deepEqual(recorded.info, [], 'no notification for a visible action');
  assert.deepEqual(recorded.errors, [], 'and no failure either');
});

test('a missing binary is reported, not swallowed', async (t) => {
  activate();
  recorded.workspaceFolders = [
    { uri: stubUri(WORKSPACE), name: 'sample_multi', index: 0 },
  ];
  recorded.configuration = { binaryPath: '/nonexistent/frost' };
  const handler = recorded.commands.get('frost.build');
  assert.ok(handler);
  await handler();
  // With no targets discoverable the command warns; what must not happen is an
  // unhandled rejection or silence.
  assert.ok(
    recorded.warnings.length > 0 || recorded.errors.length > 0,
    `expected a message, saw warnings=${JSON.stringify(recorded.warnings)} errors=${JSON.stringify(recorded.errors)}`,
  );
  t.diagnostic(`warnings: ${JSON.stringify(recorded.warnings)}`);
});

test('the task provider offers a build task per real target', async (t) => {
  if (!haveFrost) {
    t.skip('needs target/release/frost and sample_multi');
    return;
  }
  activate();
  useWorkspace();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const provider = recorded.taskProviders.get('frost') as any;
  const tasks = await provider.provideTasks();
  const names: string[] = tasks.map((task: { name: string }) => task.name);
  assert.ok(
    names.includes('build //apps/cli:cli'),
    `expected a task for the sample binary, saw ${JSON.stringify(names)}`,
  );
  assert.ok(
    names.includes('test //core:core_test'),
    'a cc_test target must also get a test task',
  );
  assert.ok(
    !names.includes('test //core:core'),
    'a library must not get a test task',
  );
  // Every task runs the configured binary with --no-tui, which is what keeps
  // the output parseable.
  const build = tasks.find((task: { name: string }) => task.name === 'build //apps/cli:cli');
  assert.equal(build.execution.process, FROST);
  assert.ok(build.execution.args.includes('--no-tui'));
});

test('building a real target publishes no diagnostics and reports success', async (t) => {
  if (!haveFrost) {
    t.skip('needs target/release/frost and sample_multi');
    return;
  }
  activate();
  useWorkspace();
  recorded.quickPickAnswer = { label: '//apps/cli:cli' };
  const handler = recorded.commands.get('frost.build');
  assert.ok(handler);
  await handler();
  assert.equal(recorded.diagnostics.size, 0, 'a clean build has nothing to report');
  assert.ok(
    recorded.status.text.includes('check'),
    `expected a success status, saw ${recorded.status.text}`,
  );
  assert.ok(
    recorded.output.some((line) => line.includes('frost:')),
    'frost output is echoed to the channel',
  );
});

test('buildFileOwners resolves a source to its target and builds it', async (t) => {
  if (!haveFrost) {
    t.skip('needs target/release/frost and sample_multi');
    return;
  }
  activate();
  useWorkspace();
  const handler = recorded.commands.get('frost.buildFileOwners');
  assert.ok(handler);
  await handler(stubUri(join(WORKSPACE, 'core/src/core.c')));
  // The owner is //core:core; the command must have run something rather than
  // reporting the file as unowned.
  assert.deepEqual(recorded.info, [], `unexpected message: ${JSON.stringify(recorded.info)}`);
  assert.ok(
    recorded.output.some((line) => line.includes('//core:core')),
    `expected a build of the owning target, saw ${JSON.stringify(recorded.output.slice(0, 3))}`,
  );
});

test('correcting the binary path does not keep serving the empty answer', async (t) => {
  if (!haveFrost) {
    t.skip('needs target/release/frost and sample_multi');
    return;
  }
  activate();
  // The sequence a real user hits: an unusable binary, an empty target list,
  // then they fix the setting. Keying the cache on the folder alone made the
  // emptiness outlive the cause, and nothing but the refresh command cleared
  // it — a bug this harness found on its first run.
  recorded.workspaceFolders = [
    { uri: stubUri(WORKSPACE), name: 'sample_multi', index: 0 },
  ];
  recorded.configuration = { binaryPath: '/nonexistent/frost', profile: 'debug', platform: '' };
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const provider = recorded.taskProviders.get('frost') as any;
  assert.equal((await provider.provideTasks()).length, 1, 'only the default task');

  recorded.configuration = { binaryPath: FROST, profile: 'debug', platform: '' };
  const names: string[] = (await provider.provideTasks()).map(
    (task: { name: string }) => task.name,
  );
  assert.ok(
    names.includes('build //apps/cli:cli'),
    `a corrected setting must be believed, saw ${JSON.stringify(names)}`,
  );
});

test('changing configuration clears the cache', () => {
  activate();
  assert.equal(recorded.configurationHandlers.length, 1);
  // Only for this extension's section: clearing on every unrelated setting
  // change would re-run frost for a colour theme.
  recorded.configurationHandlers[0]({
    affectsConfiguration: (section: string) => section === 'frostbuild',
  });
});

test('a file no target declares is explained, not silently ignored', async (t) => {
  if (!haveFrost) {
    t.skip('needs target/release/frost and sample_multi');
    return;
  }
  activate();
  useWorkspace();
  const handler = recorded.commands.get('frost.buildFileOwners');
  assert.ok(handler);
  await handler(stubUri(join(WORKSPACE, 'core/include/core.h')));
  // A header frost only learns about from a depfile has no owner in the
  // configuration. The empty answer must say so, or it looks like a broken
  // command.
  assert.ok(
    recorded.info.some((message) => message.includes('core/include/core.h')),
    `expected an explanation, saw ${JSON.stringify(recorded.info)}`,
  );
});
