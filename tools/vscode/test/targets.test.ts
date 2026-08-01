// Tests for the pure target parser and tree model. `node:test` and
// `node:assert/strict` only — the module under test never touches `vscode`, so
// there is nothing here that needs a downloaded editor to run.

import * as assert from 'node:assert/strict';
import { test } from 'node:test';

import type { LabeledTarget, TargetKind } from '../src/frost/types';
import {
  buildTargetTree,
  isRunnableKind,
  isTestKind,
  parseLabel,
  parseLabelKind,
  type TargetTreeNode,
} from '../src/frost/targets';

/**
 * Verbatim `frost -C sample_multi query deps //apps/cli:cli --output
 * label-kind` output, kept byte-for-byte in sync with the assertion in
 * `crates/frostbuild-cli/tests/e2e.rs`. If frost's format changes, that test
 * fails on the Rust side and this one on the TypeScript side.
 */
const SAMPLE = [
  'cc_binary target //apps/cli:cli',
  'cc_library target //core:core',
  'cc_library target //render:render',
  'cc_library target //text:text',
  'genrule target gen_version',
  '',
].join('\n');

const SAMPLE_TARGETS: LabeledTarget[] = [
  {
    kind: 'cc_binary',
    label: '//apps/cli:cli',
    packagePath: 'apps/cli',
    name: 'cli',
  },
  { kind: 'cc_library', label: '//core:core', packagePath: 'core', name: 'core' },
  {
    kind: 'cc_library',
    label: '//render:render',
    packagePath: 'render',
    name: 'render',
  },
  { kind: 'cc_library', label: '//text:text', packagePath: 'text', name: 'text' },
  {
    kind: 'genrule',
    label: 'gen_version',
    packagePath: '',
    name: 'gen_version',
  },
];

/**
 * Every kind in `TargetKind`, so the predicate tests can be exhaustive.
 *
 * Derived from an object checked against `Record<TargetKind, true>` rather
 * than written as a plain array, because an array of kinds silently stays
 * valid when a kind is added to `TargetKind` — and a kind nobody classified is
 * exactly the thing these tests exist to catch. This way the file stops
 * compiling until the new kind is listed and the assertions below are revised.
 */
const EVERY_KIND = {
  cc_binary: true,
  cc_library: true,
  cc_test: true,
  genrule: true,
  test: true,
  kofun_binary: true,
  command: true,
} satisfies Record<TargetKind, true>;

const ALL_KINDS = Object.keys(EVERY_KIND) as (keyof typeof EVERY_KIND)[];

test('parses the sample workspace output', () => {
  // Whole-array equality rather than field spot-checks: a regression that
  // swaps packagePath and name, or drops the verbatim label, shows up here.
  assert.deepEqual(parseLabelKind(SAMPLE), SAMPLE_TARGETS);
});

test('parses CRLF output identically', () => {
  // frost's stdout arrives CRLF-terminated on Windows. A `split('\n')` that
  // does not strip the `\r` leaves it glued to the label, and every label the
  // extension hands back to frost then resolves to nothing — a failure mode
  // that only appears on one platform, so it needs a test on all of them.
  assert.deepEqual(parseLabelKind(SAMPLE.replace(/\n/g, '\r\n')), SAMPLE_TARGETS);
});

test('ignores blank and whitespace-only lines', () => {
  const padded = `\n   \n${SAMPLE}\n\t\n\n`;
  assert.deepEqual(parseLabelKind(padded), SAMPLE_TARGETS);
  assert.deepEqual(parseLabelKind(''), []);
  assert.deepEqual(parseLabelKind('\n\n'), []);
});

test('skips an unrecognized kind and keeps the surrounding lines', () => {
  // `unknown target <name>` is not hypothetical: frost prints it for a name it
  // has no target record for. `rust_binary` stands in for a kind a future
  // frost adds. Either way the rest of the tree must still render.
  const text = [
    'cc_binary target //apps/cli:cli',
    'unknown target //ghost:ghost',
    'rust_binary target //rs:rs',
    'genrule target gen_version',
  ].join('\n');

  const parsed = parseLabelKind(text);
  assert.deepEqual(
    parsed.map((target) => target.label),
    ['//apps/cli:cli', 'gen_version'],
  );
  // The skip must be a skip, not a coercion: no parsed target may carry a kind
  // outside TargetKind, or a "Run" command gets built for something that is
  // not runnable.
  for (const target of parsed) {
    assert.ok(ALL_KINDS.includes(target.kind), `leaked kind ${target.kind}`);
  }
});

test('skips malformed lines', () => {
  const text = [
    'cc_binary target //apps/cli:cli',
    '//core:core', // no kind and no separator at all
    'cc_library //core:core', // separator word missing
    'cc_library targets //core:core', // near-miss separator
    'cc_library target', // separator present, label missing
    'cc_library target //core:core extra', // trailing field
    'cc_library target //core:', // a package with no target name
    'frost: 3 targets', // a summary line that wandered into stdout
    'genrule target gen_version',
  ].join('\n');

  assert.deepEqual(
    parseLabelKind(text).map((target) => target.label),
    ['//apps/cli:cli', 'gen_version'],
  );
});

test('preserves input order', () => {
  // The tree sorts, the parser does not. Callers that diff two query results
  // depend on frost's own ordering surviving the parse.
  const text = [
    'cc_library target //text:text',
    'genrule target gen_version',
    'cc_binary target //apps/cli:cli',
  ].join('\n');

  assert.deepEqual(
    parseLabelKind(text).map((target) => target.name),
    ['text', 'gen_version', 'cli'],
  );
});

test('parseLabel handles every label shape', () => {
  assert.deepEqual(parseLabel('//apps/cli:cli'), {
    packagePath: 'apps/cli',
    name: 'cli',
  });
  assert.deepEqual(parseLabel('gen_version'), {
    packagePath: '',
    name: 'gen_version',
  });
  // frost rewrites `//:name` to a bare name before the graph sees it, so this
  // shape never comes out of `--output label-kind`. It does appear in hand-
  // written manifests, and reading it as a root target is the only sane answer.
  assert.deepEqual(parseLabel('//:root_target'), {
    packagePath: '',
    name: 'root_target',
  });
});

test('parseLabel does not invent a package for a colon-less path label', () => {
  // Bazel expands `//apps/cli` to `//apps/cli:cli`; frost does not. Adopting
  // Bazel's shorthand here would file the target under a package frost has
  // never heard of.
  assert.deepEqual(parseLabel('//apps/cli'), {
    packagePath: '',
    name: 'apps/cli',
  });
});

/** Find a child by display name, failing the test rather than returning undefined. */
function child(node: TargetTreeNode, displayName: string): TargetTreeNode {
  const found = node.children.find((entry) => entry.displayName === displayName);
  if (found === undefined) {
    const parent = node.packagePath === '' ? '//' : node.packagePath;
    throw new Error(`expected a child named ${displayName} under ${parent}`);
  }
  return found;
}

test('buildTargetTree groups the sample by package', () => {
  const root = buildTargetTree(SAMPLE_TARGETS);

  assert.equal(root.packagePath, '');
  assert.equal(root.displayName, '//');
  // The root package owns the bare-labelled target and nothing else.
  assert.deepEqual(
    root.targets.map((target) => target.name),
    ['gen_version'],
  );
  // Children are display-name ordered, not input ordered.
  assert.deepEqual(
    root.children.map((node) => node.displayName),
    ['apps', 'core', 'render', 'text'],
  );

  const apps = child(root, 'apps');
  // `apps` declares nothing itself; it exists only to reach `apps/cli`.
  assert.deepEqual(apps.targets, []);
  assert.deepEqual(
    apps.children.map((node) => node.displayName),
    ['cli'],
  );

  const cli = child(apps, 'cli');
  assert.equal(cli.packagePath, 'apps/cli');
  assert.deepEqual(cli.children, []);
  assert.deepEqual(
    cli.targets.map((target) => target.label),
    ['//apps/cli:cli'],
  );
});

test('buildTargetTree is independent of input order', () => {
  // The strongest guard against accidental insertion-order dependence: the
  // same set fed in reverse must produce an identical tree, or the sidebar
  // reshuffles whenever a different query populates it.
  const forward = buildTargetTree(SAMPLE_TARGETS);
  const reversed = buildTargetTree([...SAMPLE_TARGETS].reverse());
  assert.deepEqual(reversed, forward);
});

test('buildTargetTree sorts sibling targets by name', () => {
  const targets = parseLabelKind(
    [
      'cc_library target //core:zeta',
      'cc_binary target //core:alpha',
      'genrule target //core:mid',
    ].join('\n'),
  );

  const core = child(buildTargetTree(targets), 'core');
  assert.deepEqual(
    core.targets.map((target) => target.name),
    ['alpha', 'mid', 'zeta'],
  );
});

test('buildTargetTree synthesizes intermediate packages', () => {
  // Only the leaf declares anything; a → b must still exist or the tree has no
  // path to c.
  const root = buildTargetTree(parseLabelKind('cc_binary target //a/b/c:x'));

  const a = child(root, 'a');
  assert.equal(a.packagePath, 'a');
  assert.deepEqual(a.targets, []);

  // Each node carries the full path, not just its own segment — the sidebar
  // builds `frost` command lines out of it.
  const b = child(a, 'b');
  assert.equal(b.packagePath, 'a/b');
  assert.deepEqual(b.targets, []);

  const c = child(b, 'c');
  assert.equal(c.packagePath, 'a/b/c');
  assert.deepEqual(c.children, []);
  assert.deepEqual(
    c.targets.map((target) => target.name),
    ['x'],
  );
});

test('buildTargetTree reuses an intermediate package for siblings', () => {
  // `apps` must be created once, not once per descendant: two nodes with the
  // same package path would show the sidebar a duplicate row.
  const root = buildTargetTree(
    parseLabelKind(
      [
        'cc_binary target //apps/cli:cli',
        'cc_binary target //apps/gui:gui',
        'cc_library target //apps:shared',
      ].join('\n'),
    ),
  );

  assert.deepEqual(
    root.children.map((node) => node.displayName),
    ['apps'],
  );
  const apps = child(root, 'apps');
  // An intermediate package that also declares its own target keeps both.
  assert.deepEqual(
    apps.targets.map((target) => target.name),
    ['shared'],
  );
  assert.deepEqual(
    apps.children.map((node) => node.packagePath),
    ['apps/cli', 'apps/gui'],
  );
});

test('buildTargetTree returns a bare root for no targets', () => {
  assert.deepEqual(buildTargetTree([]), {
    packagePath: '',
    displayName: '//',
    targets: [],
    children: [],
  });
});

test('isTestKind covers exactly the kinds frost test runs', () => {
  // Asserted over the whole kind set rather than the two positives, so a kind
  // that starts answering true here fails the test instead of quietly gaining
  // a Test Explorer entry. Sorted so the result does not depend on key order.
  assert.deepEqual(ALL_KINDS.filter(isTestKind).sort(), ['cc_test', 'test']);
});

test('isRunnableKind covers exactly the kinds that produce a launchable binary', () => {
  assert.deepEqual(ALL_KINDS.filter(isRunnableKind).sort(), [
    'cc_binary',
    'kofun_binary',
  ]);
});

test('test and runnable kinds do not overlap', () => {
  // cc_test links a binary but must be launched through `frost test`, so the
  // editor gets one affordance per target, never both.
  for (const kind of ALL_KINDS) {
    assert.ok(
      !(isTestKind(kind) && isRunnableKind(kind)),
      `${kind} is classified as both a test and a runnable`,
    );
  }
});
