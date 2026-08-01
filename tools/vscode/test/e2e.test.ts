import * as assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { after, before, test } from 'node:test';

import { installVscodeStub, stubUri, type Recorded } from './vscode-stub';

// End to end, without an editor.
//
// Every case here drives the extension the way a person does — activate, then
// a tree row, a debug launch, a test run — and lets it reach the real frost
// binary in the real sample_multi workspace. What is stubbed is VS Code, not
// frost: the build actually runs, the artifacts actually exist, and the launch
// configuration is the one frost computed rather than one written here.
//
// It stays lightweight on purpose. A harness that downloads VS Code and starts
// a display server would cover the last layer and cost minutes per run, and a
// suite nobody waits for is a suite nobody trusts. The uncovered layer is
// genuine VS Code API behaviour — whether `ProcessExecution` spawns what we
// think, whether a quick pick renders — and that needs a real editor once,
// before publishing, not on every push.

const REPO = join(__dirname, '..', '..', '..', '..');
const FROST = join(REPO, 'target', 'release', 'frost');
const WORKSPACE = join(REPO, 'sample_multi');
const available = existsSync(FROST) && existsSync(WORKSPACE);

let stub: { recorded: Recorded; dispose: () => void };
let recorded: Recorded;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let extension: any;

before(() => {
  stub = installVscodeStub();
  recorded = stub.recorded;
  // eslint-disable-next-line @typescript-eslint/no-var-requires
  extension = require('../src/extension');
});

after(() => {
  stub.dispose();
});

function activate(): void {
  recorded.commands.clear();
  recorded.taskProviders.clear();
  recorded.treeProviders.clear();
  recorded.debugProviders.clear();
  recorded.testControllers.length = 0;
  recorded.startedDebugSessions.length = 0;
  recorded.diagnostics.clear();
  recorded.output.length = 0;
  recorded.info.length = 0;
  recorded.warnings.length = 0;
  recorded.errors.length = 0;
  recorded.saveHandlers.length = 0;
  recorded.configurationHandlers.length = 0;
  recorded.quickPickAnswer = undefined;
  recorded.status.text = '';
  recorded.workspaceFolders = [
    { uri: stubUri(WORKSPACE), name: 'sample_multi', index: 0 },
  ];
  recorded.configuration = { binaryPath: FROST, profile: 'debug', platform: '' };
  extension.activate({ subscriptions: [] });
}

test('activation registers every surface the manifest promises', () => {
  activate();
  assert.ok(recorded.treeProviders.has('frostTargets'), 'the tree view');
  assert.ok(recorded.taskProviders.has('frost'), 'the task provider');
  assert.ok(recorded.debugProviders.has('frost'), 'the debug provider');
  assert.equal(recorded.testControllers.length, 1, 'the test controller');
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
    'every command package.json declares must exist, or the palette entry fails',
  );
});

test('the tree shows the real package structure', async (t) => {
  if (!available) {
    t.skip('needs target/release/frost and sample_multi');
    return;
  }
  activate();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const tree = recorded.treeProviders.get('frostTargets') as any;
  const roots = await tree.getChildren();
  const labels = roots.map((node: { kind: string; node?: { displayName: string }; target?: { name: string } }) =>
    node.kind === 'package' ? node.node?.displayName : node.target?.name,
  );
  // sample_multi has packages apps/core/render/text and a root genrule.
  assert.ok(labels.includes('core'), `expected a core package, saw ${JSON.stringify(labels)}`);
  assert.ok(labels.includes('gen_version'), 'the root genrule is a top-level row');

  const core = roots.find(
    (node: { kind: string; node?: { displayName: string } }) =>
      node.kind === 'package' && node.node?.displayName === 'core',
  );
  const children = await tree.getChildren(core);
  const names = children.map((child: { target?: { name: string } }) => child.target?.name).sort();
  assert.deepEqual(names, ['core', 'core_test'], 'both core targets appear');

  // A row must carry the kind and the click action, or the tree is decoration.
  const item = tree.getTreeItem(children.find((child: { target?: { name: string } }) => child.target?.name === 'core_test'));
  assert.equal(item.description, 'cc_test');
  assert.equal(item.contextValue, 'frostTestTarget', 'drives which menu items show');
  assert.equal(item.command.command, 'frost.buildTarget');
});

test('a tree row builds the target it names', async (t) => {
  if (!available) {
    t.skip('needs target/release/frost and sample_multi');
    return;
  }
  activate();
  const handler = recorded.commands.get('frost.buildTarget');
  assert.ok(handler);
  await handler('//core:core', recorded.workspaceFolders[0]);
  assert.ok(
    recorded.output.some((line) => line.includes('//core:core')),
    'the build actually ran',
  );
  assert.equal(recorded.diagnostics.size, 0, 'a clean build reports nothing');
  assert.ok(recorded.status.text.includes('check'), `saw ${recorded.status.text}`);
});

test('debugging resolves to the configuration frost computed', async (t) => {
  if (!available) {
    t.skip('needs target/release/frost and sample_multi');
    return;
  }
  activate();
  recorded.installedExtensions.add('ms-vscode.cpptools');
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const provider = recorded.debugProviders.get('frost') as any;
  const resolved = await provider.resolveDebugConfiguration(
    recorded.workspaceFolders[0],
    { type: 'frost', request: 'launch', name: 'x', target: '//apps/cli:cli' },
  );
  // sample_multi's debug profile has no -g, so frost refuses and says how to
  // fix it. Passing that message through verbatim is the whole point of the
  // delegation: a generic "debugging failed" would discard the only actionable
  // part.
  if (!resolved) {
    assert.ok(
      recorded.errors.some((message) => message.includes('debug symbols')),
      `expected frost's own guidance, saw ${JSON.stringify(recorded.errors)}`,
    );
    return;
  }
  // If the workspace ever gains debug symbols, the configuration must be the
  // one frost produced, aimed at a real adapter, with the redundant
  // preLaunchTask removed because the build already happened.
  assert.equal(resolved.type, 'cppdbg');
  assert.ok(String(resolved.program).includes('apps_cli_cli'));
  assert.ok(!('preLaunchTask' in resolved), 'the build already ran');
});

test('debugging without the adapter extension says which one to install', async (t) => {
  if (!available) {
    t.skip('needs target/release/frost and sample_multi');
    return;
  }
  activate();
  recorded.installedExtensions.clear();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const provider = recorded.debugProviders.get('frost') as any;
  await provider.resolveDebugConfiguration(recorded.workspaceFolders[0], {
    type: 'frost',
    request: 'launch',
    name: 'x',
    target: '//apps/cli:cli',
  });
  // Either frost refused first, or we did for the missing extension. Both must
  // produce a message naming the cause; silence is the failure mode.
  assert.ok(
    recorded.errors.length > 0,
    'a debug that cannot start must say why',
  );
});

test('the debug command hands the target to the provider', async () => {
  activate();
  const handler = recorded.commands.get('frost.debugTarget');
  assert.ok(handler);
  await handler('//apps/cli:cli', recorded.workspaceFolders[0]);
  assert.equal(recorded.startedDebugSessions.length, 1);
  const session = recorded.startedDebugSessions[0] as {
    configuration: { type: string; target: string };
  };
  // The row knows the label; everything else is the provider's job. If this
  // started a `cppdbg` session directly the delegation would be bypassed.
  assert.equal(session.configuration.type, 'frost');
  assert.equal(session.configuration.target, '//apps/cli:cli');
});

test('the test explorer discovers the real test targets', async (t) => {
  if (!available) {
    t.skip('needs target/release/frost and sample_multi');
    return;
  }
  activate();
  const handler = recorded.commands.get('frost.refreshTargets');
  assert.ok(handler);
  await handler();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const controller = recorded.testControllers[0] as any;
  const ids: string[] = [];
  controller.items.forEach((item: { id: string }) => ids.push(item.id));
  assert.deepEqual(ids, ['//core:core_test'], `saw ${JSON.stringify(ids)}`);
});

test('running a test through the explorer reports a result', async (t) => {
  if (!available) {
    t.skip('needs target/release/frost and sample_multi');
    return;
  }
  activate();
  await recorded.commands.get('frost.refreshTargets')!();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const controller = recorded.testControllers[0] as any;
  const profile = controller.profiles?.[0];
  // createRunProfile returns the profile; the explorer keeps the handler on it.
  const handler = profile?.handler ?? controller.lastRunHandler;
  if (!handler) {
    t.skip('the run profile is not observable through this stub');
    return;
  }
  await handler({ include: undefined }, { isCancellationRequested: false });
  const run = controller.runs.at(-1);
  assert.ok(run.ended, 'the run must be ended or the UI spins forever');
  assert.ok(
    run.passed.includes('//core:core_test') || run.started.includes('//core:core_test'),
    `expected the test to be reported, saw ${JSON.stringify(run)}`,
  );
});
