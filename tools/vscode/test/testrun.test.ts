// Unit tests for `src/frost/testrun.ts`.
//
// Node's built-in runner, no framework: the module under test is pure by
// design, so CI runs this with nothing installed and no VS Code downloaded.
//
// Every fixture below is output measured from `target/debug/frost`, not
// invented, and each one is named with the workspace shape that produced it.
// That matters more here than in most parsers: the whole module is a reading of
// prose that `docs/28_compatibility_contract.md` explicitly does not promise,
// so a fixture that drifted from the real tool would prove nothing at all.

import * as assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  parseTestRun,
  progressDescriptionToActionId,
} from '../src/frost/testrun';
import type { TestActionResult } from '../src/frost/testrun';

/** The shard command used by the sharded fixtures, quoted as frost echoes it. */
const SHARD_CMD =
  "/bin/sh -c 'grep -qx \"$TEST_SHARD_INDEX\" failing.txt && exit 1; echo shard $TEST_SHARD_INDEX ok'";

/** A `test` target with `shard_count = 3`, run twice so everything is cached. */
const ALL_CACHED = [
  'frost: up to date · 6 of 12 actions · 1 ms',
  'tests: 0 passed, 0 failed, 1 cached',
  '',
].join('\n');

/**
 * The same target with all three shards executing and passing.
 *
 * The interleaved `shard N of 3 status=...` lines are each shard's own stdout,
 * printed under its progress line. They are load-bearing in this file: the
 * shard names its own 0-based index while the progress line above it shows the
 * 1-based display number, so the two disagree by exactly the off-by-one this
 * module exists to get right.
 */
const ALL_PASSED_SHARDED = [
  '[1/3] TEST split (shard 3/3)',
  'shard 2 of 3 status=.frost/test/debug/split/shard-2-of-3/status',
  '[2/3] TEST split (shard 2/3)',
  'shard 1 of 3 status=.frost/test/debug/split/shard-1-of-3/status',
  '[3/3] TEST split (shard 1/3)',
  'shard 0 of 3 status=.frost/test/debug/split/shard-0-of-3/status',
  'frost: 3 built · 3 actions · 4 ms',
  'tests: 3 passed, 0 failed, 0 cached',
  '',
].join('\n');

/**
 * `failing.txt` names shard 1, so one of the three fails and the run reports it
 * twice — once as `FAILED:`, once in the failure summary.
 */
const ONE_SHARD_FAILED = [
  '[1/3] TEST split (shard 3/3)',
  'shard 2 ok',
  '[2/3] TEST split (shard 1/3)',
  'shard 0 ok',
  'FAILED: TEST split (shard 2/3)',
  `command: ${SHARD_CMD}`,
  'exit: code 1',
  'frost: 1 failed, 2 built · 3 actions · 8 ms',
  'failure summary (first 10):',
  `  test:split#1/3: command: ${SHARD_CMD}`,
  'tests: 2 passed, 1 failed, 0 cached',
  '',
].join('\n');

/**
 * The immediate rerun. Nothing changed, so the two passing shards come back
 * from cache and print nothing whatsoever; only the failure runs again.
 */
const RERUN_TWO_CACHED = [
  'FAILED: TEST split (shard 2/3)',
  `command: ${SHARD_CMD}`,
  'exit: code 1',
  'frost: 1 failed, 2 cached · 3 actions · 5 ms',
  'failure summary (first 10):',
  `  test:split#1/3: command: ${SHARD_CMD}`,
  'tests: 0 passed, 1 failed, 2 cached',
  '',
].join('\n');

/** An unsharded `cc_test` in a package, failing. Note the label's `:` and `/`. */
const UNSHARDED_FAILED = [
  '[1/3] CC core/src/core_test.c (//core:core_test)',
  '[2/3] LINK //core:core_test',
  'FAILED: TEST //core:core_test',
  'command: .frost/bin/debug/core_core_test',
  'exit: code 1',
  'frost: 1 failed, 2 built · 3 actions · 79 ms',
  'failure summary (first 10):',
  '  test://core:core_test: command: .frost/bin/debug/core_core_test',
  'tests: 0 passed, 1 failed, 0 cached',
  '',
].join('\n');

/** The same target passing: three progress lines, only one of them a test. */
const UNSHARDED_PASSED = [
  '[1/3] CC core/src/core_test.c (//core:core_test)',
  '[2/3] LINK //core:core_test',
  '[3/3] TEST //core:core_test',
  'frost: 3 built · 3 actions · 52 ms',
  'tests: 1 passed, 0 failed, 0 cached',
  '',
].join('\n');

/**
 * A `cc_test` whose library does not compile.
 *
 * Two things at once, both measured: the failure summary names a *compile*
 * action, and the test itself is skipped — counted in `1 failed` on the
 * `tests:` line while appearing nowhere in the output as an action.
 */
const COMPILE_FAILED_TEST_SKIPPED = [
  'FAILED: CC core/src/core.c (core)',
  'command: cc -MD -MF .frost/obj/debug/core/core/src/core.c.o.d -c core/src/core.c -o .frost/obj/debug/core/core/src/core.c.o',
  'exit: code 1',
  "core/src/core.c: In function 'add':",
  "core/src/core.c:1:37: error: expected ';' before '}' token",
  '    1 | int add(int a, int b) { return a + b',
  '      |                                     ^',
  '      |                                     ;',
  '    2 | }',
  '      | ~',
  '[2/5] CC core/src/core_test.c (core_test)',
  'frost: 1 failed, 1 built, 3 skipped · 5 actions · 139 ms',
  'failure summary (first 10):',
  '  compile:core:core/src/core.c: command: cc -MD -MF .frost/obj/debug/core/core/src/core.c.o.d -c core/src/core.c -o .frost/obj/debug/core/core/src/core.c.o',
  'tests: 0 passed, 1 failed, 0 cached',
  '',
].join('\n');

/** The action ids of a run's results, in the order the parser produced them. */
function ids(text: string): string[] {
  return parseTestRun(text).results.map((result) => result.actionId);
}

/** The one result for an id, asserted to exist so a miss fails loudly. */
function resultFor(text: string, actionId: string): TestActionResult {
  const matches = parseTestRun(text).results.filter(
    (result) => result.actionId === actionId,
  );
  assert.equal(matches.length, 1, `expected exactly one ${actionId}`);
  const only = matches[0];
  assert.ok(only !== undefined);
  return only;
}

// --- progressDescriptionToActionId -----------------------------------------

test('progressDescriptionToActionId maps an unsharded description', () => {
  // The label contains both `:` and `/`, so anything that rebuilds the id by
  // splitting the description apart instead of taking it whole gets it wrong.
  assert.equal(
    progressDescriptionToActionId('TEST //core:core_test'),
    'test://core:core_test',
  );
  assert.equal(progressDescriptionToActionId('TEST split'), 'test:split');
});

test('progressDescriptionToActionId converts the 1-based display shard to a 0-based id', () => {
  // The headline case, and the single most error-prone line in the module.
  // `test_shards` prints `(shard {index + 1}/{total})` and names the action
  // `#{index}/{total}` from the same index, so the display number must lose
  // one on the way back. Off by one reports every shard's result against its
  // neighbour and the last one against an id that does not exist.
  assert.equal(
    progressDescriptionToActionId('TEST split (shard 1/3)'),
    'test:split#0/3',
  );
  assert.equal(
    progressDescriptionToActionId('TEST split (shard 2/3)'),
    'test:split#1/3',
  );
  assert.equal(
    progressDescriptionToActionId('TEST split (shard 3/3)'),
    'test:split#2/3',
  );
});

test('progressDescriptionToActionId maps every shard of a split one-to-one', () => {
  // Covers the conversion across a whole split rather than at its ends: an
  // implementation that clamped instead of subtracting, or that subtracted
  // from the total as well, passes the three cases above for the wrong reason.
  const total = 12;
  const seen = new Set<string>();
  for (let display = 1; display <= total; display += 1) {
    const actionId = progressDescriptionToActionId(
      `TEST split (shard ${display}/${total})`,
    );
    assert.equal(actionId, `test:split#${display - 1}/${total}`);
    seen.add(actionId as string);
  }
  // Distinct ids, so no two shards can collapse onto one test item.
  assert.equal(seen.size, total);
});

test('progressDescriptionToActionId keeps a nested package label intact', () => {
  // The label has its own `/`, which a shard split that searches for `/`
  // instead of anchoring on the trailing `(shard n/m)` would cut in half.
  assert.equal(
    progressDescriptionToActionId('TEST //tools/vscode:unit_test (shard 2/12)'),
    'test://tools/vscode:unit_test#1/12',
  );
});

test('progressDescriptionToActionId rejects non-test descriptions', () => {
  // Every one of these appears in a real `frost test` run's progress, mixed in
  // with the TEST lines. Treating any of them as a test puts a compile or link
  // action into the Test Explorer as a phantom item.
  assert.equal(progressDescriptionToActionId('CC foo.c (//x:y)'), undefined);
  assert.equal(progressDescriptionToActionId('LINK //x:y'), undefined);
  assert.equal(progressDescriptionToActionId('AR libcore.a'), undefined);
  assert.equal(progressDescriptionToActionId('GEN gen_version'), undefined);
  // Prefix-adjacent, and case-sensitive: a test's own stdout is arbitrary text.
  assert.equal(progressDescriptionToActionId('TESTING foo'), undefined);
  assert.equal(progressDescriptionToActionId('test //x:y'), undefined);
  assert.equal(progressDescriptionToActionId('TEST'), undefined);
  assert.equal(progressDescriptionToActionId('TEST '), undefined);
  assert.equal(progressDescriptionToActionId(''), undefined);
});

test('progressDescriptionToActionId rejects an impossible display shard', () => {
  // A display number is 1-based, so 0 would convert to `#-1/3` and a number
  // past the total to an index outside its own split. Both are ids frost can
  // never resolve; skipping beats asking for a build of a phantom endpoint.
  assert.equal(progressDescriptionToActionId('TEST split (shard 0/3)'), undefined);
  assert.equal(progressDescriptionToActionId('TEST split (shard 4/3)'), undefined);
});

test('progressDescriptionToActionId rejects a single-shard suffix', () => {
  // `test_shards` returns the *unsuffixed* id and description for total <= 1,
  // so `#0/1` is an endpoint that resolves to nothing. It has to be refused
  // here explicitly, because `parseTestActionId` accepts it as well-formed.
  assert.equal(progressDescriptionToActionId('TEST split (shard 1/1)'), undefined);
});

test('progressDescriptionToActionId ignores trailing whitespace', () => {
  // Output captured line-by-line can carry a stray space or a lone `\r` from a
  // CRLF stream that some caller split on `\n` alone.
  assert.equal(
    progressDescriptionToActionId('TEST split (shard 1/3)  '),
    'test:split#0/3',
  );
});

// --- parseTestRun: the four measured runs ----------------------------------

test('parseTestRun reports an all-cached run as a count and nothing else', () => {
  // The honest answer, and the one the doc comment promises: a cached action
  // prints no progress line at all, so there is no target to attribute the
  // cached result to. Inventing one would put a green tick on a test this run
  // said nothing about.
  const run = parseTestRun(ALL_CACHED);
  assert.deepEqual(run.results, []);
  assert.equal(run.cachedCount, 1);
  assert.deepEqual(run.summary, { passed: 0, failed: 0, cached: 1 });
});

test('parseTestRun reads three passing shards to their exact 0-based ids', () => {
  // Where an off-by-one hides. The ids are asserted literally rather than by
  // count, because three results with the wrong ids look perfectly healthy.
  const run = parseTestRun(ALL_PASSED_SHARDED);
  assert.deepEqual(
    run.results.map((result) => result.actionId),
    // Progress order is completion order, which frost schedules as it likes —
    // this run finished shard 3 of 3 first. The order is the documented one.
    ['test:split#2/3', 'test:split#1/3', 'test:split#0/3'],
  );
  // The same ids as a set, in case the ordering rule is ever relaxed: exactly
  // shards 0, 1 and 2 of a 3-way split, no duplicates and nothing missing.
  assert.deepEqual(
    run.results.map((result) => result.actionId).sort(),
    ['test:split#0/3', 'test:split#1/3', 'test:split#2/3'],
  );
  assert.deepEqual(
    run.results.map((result) => result.outcome),
    ['passed', 'passed', 'passed'],
  );
  assert.deepEqual(run.summary, { passed: 3, failed: 0, cached: 0 });
  assert.equal(run.cachedCount, 0);
});

test('parseTestRun carries the label and shard through from the id', () => {
  const result = resultFor(ALL_PASSED_SHARDED, 'test:split#0/3');
  assert.equal(result.label, 'split');
  assert.deepEqual(result.shard, { index: 0, total: 3 });
});

test('parseTestRun attributes each shard the output that shard printed', () => {
  // An independent check on the conversion, from the run itself rather than
  // from the fixture author: each shard prints its own TEST_SHARD_INDEX, which
  // is 0-based, under a progress line that shows the 1-based display number.
  // If the two were ever mapped together, this is where it shows.
  for (const index of [0, 1, 2]) {
    const result = resultFor(ALL_PASSED_SHARDED, `test:split#${index}/3`);
    assert.equal(
      result.detail,
      `shard ${index} of 3 status=.frost/test/debug/split/shard-${index}-of-3/status`,
    );
  }
});

test('parseTestRun separates the failing shard from the two that passed', () => {
  const run = parseTestRun(ONE_SHARD_FAILED);
  assert.deepEqual(
    run.results.map((result) => [result.actionId, result.outcome]),
    [
      // Failures first, then passes in progress order.
      ['test:split#1/3', 'failed'],
      ['test:split#2/3', 'passed'],
      ['test:split#0/3', 'passed'],
    ],
  );
  assert.deepEqual(run.summary, { passed: 2, failed: 1, cached: 0 });
  assert.equal(run.cachedCount, 0);
});

test('parseTestRun names the failing shard once, not twice', () => {
  // frost reports a failure in two places — its `FAILED:` line and the failure
  // summary — so a parser that simply appends both shows the shard twice and
  // VS Code gets two conflicting messages for one test item.
  assert.equal(ids(ONE_SHARD_FAILED).length, 3);
  const failed = parseTestRun(ONE_SHARD_FAILED).results.filter(
    (result) => result.outcome === 'failed',
  );
  assert.equal(failed.length, 1);
  assert.equal(failed[0]?.actionId, 'test:split#1/3');
});

test('parseTestRun prefers the fuller detail frost printed under FAILED', () => {
  // The failure summary is truncated to the first line of the detail by the
  // CLI itself; the `FAILED:` block has all of it. Showing the summary's line
  // when the exit status is right there would hide why the test failed.
  const result = resultFor(ONE_SHARD_FAILED, 'test:split#1/3');
  assert.equal(result.detail, `command: ${SHARD_CMD}\nexit: code 1`);
});

test('parseTestRun leaves cached shards out of a rerun entirely', () => {
  // The measured rerun: one shard still failing, two restored from cache. The
  // cached two print nothing at all, so the only honest report is their
  // absence — asserting it is the point of this test.
  const run = parseTestRun(RERUN_TWO_CACHED);
  assert.deepEqual(ids(RERUN_TWO_CACHED), ['test:split#1/3']);
  assert.equal(run.results[0]?.outcome, 'failed');
  assert.equal(run.cachedCount, 2);
  assert.deepEqual(run.summary, { passed: 0, failed: 1, cached: 2 });
  // Neither cached shard may be guessed into the results in any state.
  for (const absent of ['test:split#0/3', 'test:split#2/3']) {
    assert.equal(
      run.results.some((result) => result.actionId === absent),
      false,
      absent,
    );
  }
});

// --- parseTestRun: mixed builds --------------------------------------------

test('parseTestRun ignores the compile and link progress of a test target', () => {
  // `CC ... (//core:core_test)` and `LINK //core:core_test` name the test
  // target, so anything keying off the label rather than the `TEST` verb turns
  // one test into three results — and reports it as passed twice before it has
  // run at all.
  assert.deepEqual(ids(UNSHARDED_PASSED), ['test://core:core_test']);
  const result = resultFor(UNSHARDED_PASSED, 'test://core:core_test');
  assert.equal(result.outcome, 'passed');
  assert.equal(result.label, '//core:core_test');
  assert.equal(result.shard, undefined);
  assert.equal(result.detail, undefined);
});

test('parseTestRun reads an unsharded failure, label colons and all', () => {
  // The failure-summary entry is `test://core:core_test: command: ...`, which
  // has four colons in it. Splitting on the first or the last gives garbage;
  // only the first `": "` is the separator.
  assert.deepEqual(ids(UNSHARDED_FAILED), ['test://core:core_test']);
  const result = resultFor(UNSHARDED_FAILED, 'test://core:core_test');
  assert.equal(result.outcome, 'failed');
  assert.equal(result.label, '//core:core_test');
  assert.equal(
    result.detail,
    'command: .frost/bin/debug/core_core_test\nexit: code 1',
  );
});

test('parseTestRun does not turn a compile failure into a test result', () => {
  // Measured from a `frost test` whose library would not compile. The failure
  // summary names a compile action and the test never ran, so there is no test
  // result to report — and a `compile:` id must not be forced into one.
  const run = parseTestRun(COMPILE_FAILED_TEST_SKIPPED);
  assert.deepEqual(run.results, []);
  // The skipped test is still counted as failed by frost's own summary. That
  // gap between `summary.failed` and the results is real and documented: a
  // skipped test prints nothing, so it cannot be attributed to a target here.
  assert.deepEqual(run.summary, { passed: 0, failed: 1, cached: 0 });
  assert.equal(run.cachedCount, 0);
});

test('parseTestRun rejects a packaged compile id in the failure summary', () => {
  // The package-qualified spelling, `compile://core:core:core/src/core.c`,
  // contains `test`-free but colon-heavy text and is the shape most likely to
  // fool a loose prefix check.
  const output = [
    'frost: 2 failed · 4 actions · 6 ms',
    'failure summary (first 10):',
    '  compile://core:core:core/src/core.c: command: cc -c core/src/core.c',
    '  link://core:core_test: command: cc -o core_test',
    '  test://core:core_test: command: .frost/bin/debug/core_core_test',
    'tests: 0 passed, 1 failed, 0 cached',
    '',
  ].join('\n');
  assert.deepEqual(ids(output), ['test://core:core_test']);
});

test('parseTestRun does not attribute a compiler diagnostic to a test', () => {
  // The lines under a `FAILED: CC ...` are diagnostics, which `diagnostics.ts`
  // owns. If the sink stayed open across a non-test framing line they would be
  // appended to whichever test was framed last, and the Test Explorer would
  // show a C error as a test's output.
  const run = parseTestRun(COMPILE_FAILED_TEST_SKIPPED);
  assert.deepEqual(run.results, []);
  const mixed = [
    '[1/4] TEST //core:core_test',
    'core_test: 4 assertions, all ok',
    '[2/4] CC render/src/render.c (//render:render)',
    'render/src/render.c:9:5: warning: unused variable',
    'frost: 4 built · 4 actions · 12 ms',
    'tests: 1 passed, 0 failed, 0 cached',
    '',
  ].join('\n');
  const result = resultFor(mixed, 'test://core:core_test');
  assert.equal(result.detail, 'core_test: 4 assertions, all ok');
});

// --- parseTestRun: block boundaries and framing ----------------------------

test('parseTestRun closes the failure block at the tests: line', () => {
  // The block ends at the first unindented line. Carrying on past it would
  // read `tests: 2 passed...` as an action id, and would then absorb whatever
  // indented line came next — here a stray continuation that names a test.
  const output = [
    'frost: 1 failed, 2 built · 3 actions · 7 ms',
    'failure summary (first 10):',
    `  test:split#1/3: command: ${SHARD_CMD}`,
    'tests: 2 passed, 1 failed, 0 cached',
    '  test:split#2/3: not part of the block',
    '',
  ].join('\n');
  assert.deepEqual(ids(output), ['test:split#1/3']);
  assert.deepEqual(parseTestRun(output).summary, {
    passed: 2,
    failed: 1,
    cached: 0,
  });
});

test('parseTestRun keeps the summary when the block ends the output', () => {
  // The mirror of the test above: the summary line has to survive the block
  // parsing, or a failing run reports its failures with no counts at all.
  const run = parseTestRun(ONE_SHARD_FAILED);
  assert.deepEqual(run.summary, { passed: 2, failed: 1, cached: 0 });
});

test('parseTestRun reads a failure the first-10 cap dropped', () => {
  // `main.rs` truncates the failure summary at ten entries. Past the cap the
  // only record of a failing action is its `FAILED:` line, and without reading
  // those the eleventh test would vanish from the results while the `tests:`
  // line still counted it — a test item left looking un-run after a red build.
  const total = 11;
  const lines: string[] = [];
  for (let display = 1; display <= total; display += 1) {
    lines.push(`FAILED: TEST split (shard ${display}/${total})`);
    lines.push('exit: code 1');
  }
  lines.push(`frost: ${total} failed · ${total} actions · 20 ms`);
  lines.push('failure summary (first 10):');
  for (let index = 0; index < 10; index += 1) {
    lines.push(`  test:split#${index}/${total}: exit: code 1`);
  }
  lines.push(`tests: 0 passed, ${total} failed, 0 cached`);
  lines.push('');

  const run = parseTestRun(lines.join('\n'));
  assert.equal(run.results.length, total);
  assert.deepEqual(
    run.results.map((result) => result.outcome),
    new Array<string>(total).fill('failed'),
  );
  // The capped one is last, since the summary's ten come first in block order.
  assert.equal(run.results[total - 1]?.actionId, `test:split#10/${total}`);
});

test('parseTestRun never reports a failing action as passed', () => {
  // The one error that lets a red run look finished. Frost prints `FAILED:`
  // instead of a progress line, so this cannot happen today — the assertion is
  // here so a future reading of the progress lines cannot make it happen.
  const output = [
    '[1/2] TEST split (shard 1/2)',
    'FAILED: TEST split (shard 2/2)',
    'exit: code 1',
    '[2/2] TEST split (shard 2/2)',
    'frost: 1 failed, 1 built · 2 actions · 3 ms',
    'failure summary (first 10):',
    '  test:split#1/2: exit: code 1',
    'tests: 1 passed, 1 failed, 0 cached',
    '',
  ].join('\n');
  const result = resultFor(output, 'test:split#1/2');
  assert.equal(result.outcome, 'failed');
  assert.deepEqual(ids(output), ['test:split#1/2', 'test:split#0/2']);
});

test('parseTestRun reads the excerpt of a failing run that has no progress lines', () => {
  // The tail of the failing run on its own, which is how the output is often
  // quoted. Only the failure is recoverable from it: passes come exclusively
  // from progress lines, so with none present there are none to report even
  // though the summary says two tests passed. Absence is the honest answer.
  const excerpt = [
    'frost: 1 failed, 2 built · 3 actions · 7 ms',
    'failure summary (first 10):',
    `  test:split#1/3: command: ${SHARD_CMD}`,
    'tests: 2 passed, 1 failed, 0 cached',
    '',
  ].join('\n');
  const run = parseTestRun(excerpt);
  assert.deepEqual(ids(excerpt), ['test:split#1/3']);
  assert.equal(run.results[0]?.outcome, 'failed');
  // The detail falls back to the summary entry when there is no FAILED block.
  assert.equal(run.results[0]?.detail, `command: ${SHARD_CMD}`);
  assert.deepEqual(run.summary, { passed: 2, failed: 1, cached: 0 });
});

// --- parseTestRun: shapes with nothing in them -----------------------------

test('parseTestRun returns no summary for a run that printed none', () => {
  // A plain `frost build` has no `tests:` line, and that has to stay
  // distinguishable from a test run that reported zeros — otherwise the UI
  // shows a test count for a build that had no tests in it.
  const buildOnly = [
    '[1/2] CC core/src/core.c (//core:core)',
    '[2/2] LINK //core:core',
    'frost: 2 built · 2 actions · 30 ms',
    '',
  ].join('\n');
  const run = parseTestRun(buildOnly);
  assert.deepEqual(run.results, []);
  assert.equal(run.summary, undefined);
  // No summary means no cached count to report, not a cached count of nothing
  // — but the field is a number, so zero is the only available spelling.
  assert.equal(run.cachedCount, 0);
});

test('parseTestRun handles empty input', () => {
  assert.deepEqual(parseTestRun(''), { results: [], cachedCount: 0 });
});

test('parseTestRun handles CRLF input', () => {
  // Output captured on Windows arrives CRLF-terminated. Splitting on `\n`
  // alone leaves a `\r` on the end of every description, so `(shard 1/3)\r`
  // stops matching and every shard silently disappears from the results.
  const crlf = ONE_SHARD_FAILED.split('\n').join('\r\n');
  assert.deepEqual(ids(crlf), ids(ONE_SHARD_FAILED));
  assert.deepEqual(
    parseTestRun(crlf).summary,
    parseTestRun(ONE_SHARD_FAILED).summary,
  );
  // The detail must not keep a stray `\r` either; it goes straight into the UI.
  assert.equal(
    resultFor(crlf, 'test:split#1/3').detail,
    `command: ${SHARD_CMD}\nexit: code 1`,
  );
});

test('parseTestRun is not stateful across calls', () => {
  // The module holds `/g`-free regexes on purpose, but the summary it delegates
  // to does not. A `lastIndex` leaking anywhere makes every second call in a
  // session come back wrong, which is the hardest kind of bug to see.
  const first = parseTestRun(ONE_SHARD_FAILED);
  const second = parseTestRun(ONE_SHARD_FAILED);
  assert.deepEqual(second, first);
});
