// Unit tests for `src/frost/tests.ts`.
//
// Node's built-in runner, no framework: the module under test is pure by
// design, so CI runs this with nothing installed and no VS Code downloaded.
//
// The fixtures are output measured from the real tool, not invented. The
// summary strings match `println!` in `crates/frostbuild-cli/src/main.rs`, and
// the action ids match `test_shards` in `crates/frostbuild-core/src/graph.rs`.

import * as assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  buildTestItems,
  mergeObservedShards,
  parseTestActionId,
  parseTestSummary,
} from '../src/frost/tests';
import type { LabeledTarget, TargetKind, TestItem } from '../src/frost/types';

function target(
  kind: TargetKind,
  label: string,
  packagePath: string,
  name: string,
): LabeledTarget {
  return { kind, label, packagePath, name };
}

/** Build one item through the real code path, so fixtures cannot drift from it. */
function item(label: string, shardCount?: number): TestItem {
  // `//pkg:name` splits into its two halves; a bare root label is all name.
  const colon = label.lastIndexOf(':');
  const packagePath = colon === -1 ? '' : label.slice(2, colon);
  const name = colon === -1 ? label : label.slice(colon + 1);
  const built = buildTestItems(
    [target('test', label, packagePath, name)],
    shardCount === undefined ? undefined : new Map([[label, shardCount]]),
  );
  assert.equal(built.length, 1);
  // Non-null asserted through a local so the fixture fails loudly rather than
  // producing an `undefined` item that makes the real assertion vacuous.
  const only = built[0];
  assert.ok(only !== undefined);
  return only;
}

// --- parseTestSummary ------------------------------------------------------

test('parseTestSummary reads an all-cached run', () => {
  assert.deepEqual(parseTestSummary('tests: 0 passed, 0 failed, 1 cached\n'), {
    passed: 0,
    failed: 0,
    cached: 1,
  });
});

test('parseTestSummary reads a mixed pass/fail run', () => {
  assert.deepEqual(parseTestSummary('tests: 2 passed, 1 failed, 0 cached\n'), {
    passed: 2,
    failed: 1,
    cached: 0,
  });
});

test('parseTestSummary tolerates the "(no affected tests)" note', () => {
  // frost appends this note when `--affected` selected nothing. A pattern
  // anchored at the end of the line ($ right after `cached`) drops it, and the
  // extension then claims no test run happened when frost said the opposite.
  assert.deepEqual(
    parseTestSummary('tests: 0 passed, 0 failed, 0 cached (no affected tests)\n'),
    { passed: 0, failed: 0, cached: 0 },
  );
});

test('parseTestSummary takes the last summary when a buffer holds several', () => {
  // One captured buffer can hold a build and then a test, or a watch session
  // appending runs. A `.match()`-first-hit implementation reports the stale
  // counts forever, which looks exactly like a test that never re-runs.
  const text = [
    'tests: 0 passed, 1 failed, 0 cached',
    'frost: rebuilding',
    'tests: 2 passed, 0 failed, 1 cached',
    '',
  ].join('\n');
  assert.deepEqual(parseTestSummary(text), {
    passed: 2,
    failed: 0,
    cached: 1,
  });
});

test('parseTestSummary returns undefined when no summary was printed', () => {
  // A plain `frost build` prints none. That has to stay distinguishable from
  // `0 passed, 0 failed, 0 cached`, or the UI shows a test count for a run
  // that never had tests in it.
  const buildOnly = [
    'frost: 12 actions, 3 cached',
    'compile://core:core:core/src/core.c',
    'link://core:core',
    '',
  ].join('\n');
  assert.equal(parseTestSummary(buildOnly), undefined);
  assert.equal(parseTestSummary(''), undefined);
});

test('parseTestSummary handles CRLF input', () => {
  // Output captured on Windows arrives CRLF-terminated. A `/m` pattern
  // anchoring `$` after `cached` fails there, because `\r` sits in between.
  const text = 'frost: building\r\ntests: 2 passed, 1 failed, 0 cached\r\n';
  assert.deepEqual(parseTestSummary(text), {
    passed: 2,
    failed: 1,
    cached: 0,
  });
});

test('parseTestSummary reads multi-digit counts', () => {
  // A single-`\d` pattern silently truncates: 105 failed would read as 1.
  assert.deepEqual(
    parseTestSummary('tests: 12 passed, 105 failed, 7 cached\n'),
    { passed: 12, failed: 105, cached: 7 },
  );
});

test('parseTestSummary finds the summary after a failure block', () => {
  // The realistic shape of a failing `frost test`: the failure summary names
  // action ids on their own lines, and the `tests:` line follows. Nothing in
  // the failure block should be mistaken for the summary.
  const output = [
    'failure summary (first 10):',
    "  test:split#1/3: command: /bin/sh -c '...'",
    'tests: 2 passed, 1 failed, 0 cached',
    '',
  ].join('\n');
  assert.deepEqual(parseTestSummary(output), {
    passed: 2,
    failed: 1,
    cached: 0,
  });
});

test('parseTestSummary is not stateful across calls', () => {
  // A module-level `/g` regex driven by `.exec()` keeps `lastIndex` between
  // calls and returns undefined on every second call. That failure only shows
  // up on the second run of a session, which is the worst time to find it.
  const text = 'tests: 1 passed, 0 failed, 0 cached\n';
  const first = parseTestSummary(text);
  const second = parseTestSummary(text);
  assert.deepEqual(first, { passed: 1, failed: 0, cached: 0 });
  assert.deepEqual(second, first);
});

// --- parseTestActionId -----------------------------------------------------

test('parseTestActionId reads an unsharded package-qualified id', () => {
  // Splitting on the FIRST `:` yields the label `//core`; splitting on the
  // LAST yields `//core:core`. Only stripping the prefix by length is right.
  assert.deepEqual(parseTestActionId('test://core:core_test'), {
    label: '//core:core_test',
  });
});

test('parseTestActionId reads a sharded package-qualified id', () => {
  // The headline case. It catches splitting on the wrong `:` (the label has
  // two) and splitting the shard on the wrong `/` (the prefix `//` has two
  // before the shard's own).
  assert.deepEqual(parseTestActionId('test://core:core_test#1/3'), {
    label: '//core:core_test',
    shard: { index: 1, total: 3 },
  });
});

test('parseTestActionId reads shard index 0', () => {
  // 0 is falsy; an implementation testing `if (index)` drops the first shard.
  assert.deepEqual(parseTestActionId('test://core:core_test#0/3'), {
    label: '//core:core_test',
    shard: { index: 0, total: 3 },
  });
});

test('parseTestActionId reads a two-digit total', () => {
  // A single-`\d` shard pattern reads `#9/12` as total 1, which collapses a
  // twelve-way split to one shard and loses eleven twelfths of the run.
  assert.deepEqual(parseTestActionId('test://core:core_test#9/12'), {
    label: '//core:core_test',
    shard: { index: 9, total: 12 },
  });
});

test('parseTestActionId reads a bare root-package name', () => {
  // Root targets have no `//pkg:` at all; frost prints them bare, as the
  // label-kind e2e fixture (`genrule target gen_version`) shows.
  assert.deepEqual(parseTestActionId('test:split'), { label: 'split' });
  assert.deepEqual(parseTestActionId('test:split#2/3'), {
    label: 'split',
    shard: { index: 2, total: 3 },
  });
});

test('parseTestActionId keeps a nested package path intact', () => {
  // The label itself contains `/`, so a shard split that searches for `/`
  // instead of anchoring on the last `#` picks the package separator and
  // returns nonsense. This is the strongest single regression here.
  assert.deepEqual(parseTestActionId('test://tools/vscode:unit_test#1/2'), {
    label: '//tools/vscode:unit_test',
    shard: { index: 1, total: 2 },
  });
});

test('parseTestActionId rejects non-test action ids', () => {
  assert.equal(
    parseTestActionId('compile://core:core:core/src/core.c'),
    undefined,
  );
  // Contains the literal `test:` in the middle (`core_test:core`), so an
  // `includes`/`indexOf` check misidentifies a compile action as a test.
  assert.equal(
    parseTestActionId('compile://core:core_test:core/src/core_test.c'),
    undefined,
  );
  assert.equal(parseTestActionId('link://core:core_test'), undefined);
  // Prefix-adjacent but not the prefix.
  assert.equal(parseTestActionId('tests://core:core_test'), undefined);
  assert.equal(parseTestActionId('test'), undefined);
  assert.equal(parseTestActionId(''), undefined);
});

test('parseTestActionId rejects an id with no label', () => {
  assert.equal(parseTestActionId('test:'), undefined);
  assert.equal(parseTestActionId('test:#0/3'), undefined);
});

test('parseTestActionId rejects a malformed shard suffix', () => {
  // `#` cannot occur in a label, so folding a bad suffix back into the label
  // would manufacture a target name frost cannot resolve. Skipping is safer.
  assert.equal(parseTestActionId('test://core:core_test#abc'), undefined);
  assert.equal(parseTestActionId('test://core:core_test#1'), undefined);
  assert.equal(parseTestActionId('test://core:core_test#1/'), undefined);
  assert.equal(parseTestActionId('test://core:core_test#1/2/3'), undefined);
  // Impossible shards: a zero total, and an index outside its own total.
  assert.equal(parseTestActionId('test://core:core_test#0/0'), undefined);
  assert.equal(parseTestActionId('test://core:core_test#3/3'), undefined);
});

test('parseTestActionId round-trips every id buildTestItems emits', () => {
  // Locks the producer and the consumer together. If one side ever changes its
  // spelling of the shard suffix, this fails instead of the extension quietly
  // asking frost to build endpoints that do not exist.
  const label = '//tools/vscode:unit_test';
  for (const count of [1, 2, 3, 12]) {
    for (const shard of item(label, count).shards) {
      const parsed = parseTestActionId(shard.actionId);
      assert.ok(parsed !== undefined, shard.actionId);
      assert.equal(parsed.label, label);
      assert.equal(parsed.shard?.index, shard.index);
      assert.equal(parsed.shard?.total, count === 1 ? undefined : shard.total);
    }
  }
});

// --- buildTestItems --------------------------------------------------------

test('buildTestItems drops non-test kinds', () => {
  const items = buildTestItems([
    target('cc_library', '//core:core', 'core', 'core'),
    target('cc_binary', '//app:app', 'app', 'app'),
    target('genrule', 'gen_version', '', 'gen_version'),
    target('command', 'fmt', '', 'fmt'),
    target('kofun_binary', '//k:k', 'k', 'k'),
    target('cc_test', '//core:core_test', 'core', 'core_test'),
    target('test', 'split', '', 'split'),
  ]);
  assert.deepEqual(
    items.map((each) => each.label),
    ['//core:core_test', 'split'],
  );
  assert.deepEqual(
    items.map((each) => each.kind),
    ['cc_test', 'test'],
  );
});

test('buildTestItems carries the package fields through', () => {
  const items = buildTestItems([
    target('cc_test', '//core:core_test', 'core', 'core_test'),
  ]);
  assert.deepEqual(items, [
    {
      label: '//core:core_test',
      kind: 'cc_test',
      packagePath: 'core',
      name: 'core_test',
      shards: [{ actionId: 'test://core:core_test', total: 1 }],
    },
  ]);
});

test('buildTestItems assumes a single unsuffixed shard by default', () => {
  // frost's `test_shards` returns the bare id for `total <= 1`, so emitting
  // `#0/1` here would be an endpoint that resolves to nothing at all.
  const shards = item('//core:core_test').shards;
  assert.deepEqual(shards, [{ actionId: 'test://core:core_test', total: 1 }]);
  assert.equal(shards[0]?.index, undefined);
});

test('buildTestItems expands a shard count into N ids', () => {
  // Matches the ids asserted in `graph.rs`: test:split#0/3 .. #2/3.
  assert.deepEqual(item('split', 3).shards, [
    { actionId: 'test:split#0/3', index: 0, total: 3 },
    { actionId: 'test:split#1/3', index: 1, total: 3 },
    { actionId: 'test:split#2/3', index: 2, total: 3 },
  ]);
});

test('buildTestItems treats a count of 1 as unsharded', () => {
  // `shard_count = 1` is explicitly allowed in frost.toml and must produce
  // exactly the ids an omitted field does, so a journal stays valid.
  assert.deepEqual(item('split', 1).shards, [
    { actionId: 'test:split', total: 1 },
  ]);
});

test('buildTestItems degrades an unusable shard count to unsharded', () => {
  // A count out of a config file can be junk. Falling back beats throwing
  // while the test tree is being populated.
  for (const bad of [0, -3, 2.5, Number.NaN]) {
    assert.deepEqual(
      item('split', bad).shards,
      [{ actionId: 'test:split', total: 1 }],
      `count ${bad}`,
    );
  }
});

test('buildTestItems sorts by label regardless of input order', () => {
  // Query output order follows the graph, which changes as deps change; the
  // tree must not reshuffle under the user because an edge was added.
  const targets = [
    target('test', 'bare', '', 'bare'),
    target('cc_test', '//z:a', 'z', 'a'),
    target('test', '//a:z', 'a', 'z'),
  ];
  const forward = buildTestItems(targets).map((each) => each.label);
  const reversed = buildTestItems([...targets].reverse()).map(
    (each) => each.label,
  );
  // Code-unit order: '/' (0x2F) sorts before 'b' (0x62), so root-package
  // labels come last.
  assert.deepEqual(forward, ['//a:z', '//z:a', 'bare']);
  assert.deepEqual(reversed, forward);
});

test('buildTestItems ignores shard counts for labels it did not see', () => {
  const items = buildTestItems(
    [target('test', 'split', '', 'split')],
    new Map([
      ['split', 2],
      ['//gone:gone_test', 9],
    ]),
  );
  assert.deepEqual(
    items.flatMap((each) => each.shards.map((shard) => shard.actionId)),
    ['test:split#0/2', 'test:split#1/2'],
  );
});

test('buildTestItems returns an empty list for no targets', () => {
  assert.deepEqual(buildTestItems([]), []);
  assert.deepEqual(
    buildTestItems([target('cc_library', '//core:core', 'core', 'core')]),
    [],
  );
});

// --- mergeObservedShards ---------------------------------------------------

test('mergeObservedShards corrects an assumed single shard into the real three', () => {
  // The whole point of the unsharded default: the first real run tells us the
  // target was split, and the tree has to pick that up.
  const before = item('split');
  const after = mergeObservedShards(before, [
    'test:split#0/3',
    'test:split#1/3',
    'test:split#2/3',
  ]);
  assert.deepEqual(after.shards, [
    { actionId: 'test:split#0/3', index: 0, total: 3 },
    { actionId: 'test:split#1/3', index: 1, total: 3 },
    { actionId: 'test:split#2/3', index: 2, total: 3 },
  ]);
  // Identity fields survive the merge.
  assert.equal(after.label, before.label);
  assert.equal(after.kind, before.kind);
  assert.equal(after.name, before.name);
  assert.equal(after.packagePath, before.packagePath);
});

test('mergeObservedShards sorts observed shards by index', () => {
  // frost reports in completion order, which is scheduler-dependent: shard 2
  // finishing first is normal, and must not reorder the tree.
  const merged = mergeObservedShards(item('split'), [
    'test:split#2/3',
    'test:split#0/3',
    'test:split#1/3',
  ]);
  assert.deepEqual(
    merged.shards.map((shard) => shard.index),
    [0, 1, 2],
  );
});

test('mergeObservedShards deduplicates repeated ids', () => {
  // A failing shard is named twice in one run: once by progress, once by the
  // failure summary. A plain array push shows it twice in the tree.
  const merged = mergeObservedShards(item('split'), [
    'test:split#0/2',
    'test:split#1/2',
    'test:split#1/2',
  ]);
  assert.deepEqual(
    merged.shards.map((shard) => shard.actionId),
    ['test:split#0/2', 'test:split#1/2'],
  );
});

test('mergeObservedShards ignores ids for other labels', () => {
  // Prefix-overlapping labels are the trap: `//core:core_test_extra` starts
  // with `//core:core_test`, so a `startsWith` comparison merges it in.
  const merged = mergeObservedShards(item('//core:core_test'), [
    'test://core:other_test#0/2',
    'test://core:core_test_extra#1/2',
    'test://core:core_test#1/2',
    'test://core:core_test#0/2',
  ]);
  assert.deepEqual(
    merged.shards.map((shard) => shard.actionId),
    ['test://core:core_test#0/2', 'test://core:core_test#1/2'],
  );
});

test('mergeObservedShards returns the item unchanged when nothing matches', () => {
  // Identity, not just equality: the caller uses it to skip rebuilding this
  // branch of the tree after a run that touched only other targets.
  const before = item('//core:core_test', 3);
  assert.equal(
    mergeObservedShards(before, ['test://other:other_test#0/2']),
    before,
  );
  assert.equal(mergeObservedShards(before, []), before);
  // Non-test ids carry no information about shards either.
  assert.equal(
    mergeObservedShards(before, [
      'compile://core:core_test:core/src/core_test.c',
      'link://core:core_test',
    ]),
    before,
  );
});

test('mergeObservedShards collapses back to unsharded', () => {
  // The reverse correction: `shard_count` was removed from frost.toml, so the
  // run reports the bare id and the three stale shards must go.
  const before = item('split', 3);
  assert.equal(before.shards.length, 3);
  const after = mergeObservedShards(before, ['test:split']);
  assert.deepEqual(after.shards, [{ actionId: 'test:split', total: 1 }]);
  assert.equal(after.shards[0]?.index, undefined);
});

test('mergeObservedShards does not mutate the item it was given', () => {
  // The item is cached in the extension's tree; sorting or pushing in place
  // would corrupt the copy the UI is already holding.
  const before = item('split');
  const snapshot = JSON.stringify(before);
  mergeObservedShards(before, ['test:split#0/2', 'test:split#1/2']);
  assert.equal(JSON.stringify(before), snapshot);
});

test('mergeObservedShards accepts ids taken straight from real output', () => {
  // End-to-end shape check against the measured failure-summary text: the ids
  // frost prints there must feed this function without further cleaning.
  const observed = [
    'test://core:core_test#0/3',
    'test://core:core_test#1/3',
    'test://core:core_test#2/3',
  ];
  const merged = mergeObservedShards(item('//core:core_test'), observed);
  assert.deepEqual(
    merged.shards.map((shard) => shard.actionId),
    observed,
  );
  assert.deepEqual(
    merged.shards.map((shard) => shard.total),
    [3, 3, 3],
  );
});
