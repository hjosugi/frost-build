// Reading a per-test outcome out of one `frost test` run.
//
// VS Code's Test Explorer wants one verdict per test item. `tests.ts` supplies
// the run-wide `tests:` counts and the shape of an action id; neither says
// *which* target passed. This module is the missing half: it walks the progress
// text and attributes an outcome to each test action id.
//
// Pure text in, plain data out, for the same reason as its neighbours — it runs
// under `node --test` with no VS Code download and no built frost binary.
//
// Scope is per-test outcomes only. Compiler diagnostics out of the very same
// output are `diagnostics.ts`'s job: a `frost test` whose compile step fails
// produces both, and the two parsers are kept independent so a change to
// compiler prose cannot take the test tree down with it.
//
// What frost's plain renderer actually prints, per action, from
// `crates/frostbuild-cli/src/progress.rs`:
//
//   Executed  `[{completed}/{total}] {desc}`, then the action's captured output
//   Failed    `FAILED: {desc}`, then its detail
//   Cached    nothing at all
//   Skipped   nothing at all
//
// The last two lines of that table are the reason this module deliberately
// reports less than the `tests:` counts do; see `TestRunResult.cachedCount` and
// the note on `parseTestRun` about skipped tests.

import { parseTestActionId, parseTestSummary } from './tests';
import type { TestSummary } from './types';

/**
 * The two verdicts this output can support.
 *
 * There is no `skipped` member on purpose. Frost has an `Outcome::Skipped` —
 * a test whose dependency failed to build never runs — but the plain renderer
 * prints nothing for it and `main.rs` folds it into the `failed` count of the
 * `tests:` line. So a skipped test is invisible here, and inventing a third
 * verdict would imply this module can tell it apart when it cannot.
 */
export type TestOutcome = 'passed' | 'failed';

/** What one test action did, ready to be applied to a VS Code test item. */
export interface TestActionResult {
  /** Action id frost uses: `test:NAME` or `test:NAME#i/n`. */
  actionId: string;
  /** Target label, exactly as frost spells it. From `parseTestActionId`. */
  label: string;
  /** Present only for a sharded action. `index` is 0-based, as the id is. */
  shard?: { index: number; total: number };
  outcome: TestOutcome;
  /** Lines frost printed for this action, when attributable. */
  detail?: string;
}

/** Everything one run's output says about the tests in it. */
export interface TestRunResult {
  results: TestActionResult[];
  summary?: TestSummary;
  /**
   * How many tests the run served from cache, from the `tests:` line.
   *
   * A count and nothing more, because a count is all the output contains. A
   * cached action prints no progress line and no failure entry — `run_plain`
   * discards `ProgressState::Cached` without a word — so **which** targets were
   * cached is not recoverable from this text. The honest consequence is that
   * cached tests never appear in `results`; a caller that wants to show them as
   * still-green must carry the previous run's verdicts forward itself, or ask
   * frost again. Guessing here would put a green tick on a target this run said
   * nothing about.
   */
  cachedCount: number;
}

/**
 * `[3/9] TEST split (shard 1/3)` — an action finished, and what it was.
 *
 * Reading this line as "passed" is sound because `run_plain` prints it only for
 * `ProgressState::Executed`: a failure prints `FAILED:` instead, and a cached
 * or skipped action prints nothing. The counter itself is completion progress
 * over the whole run and says nothing about shards — it is discarded.
 */
const PROGRESS = /^\[\d+\/\d+\][ \t]+(.*)$/;

/** `FAILED: TEST split (shard 2/3)` — the failure counterpart of `PROGRESS`. */
const FAILED = /^FAILED:[ \t]+(.*)$/;

/**
 * `failure summary (first 10):`.
 *
 * Matched on the leading words rather than the exact count, since the cap is a
 * constant in the CLI and bumping it should not silently cost us the block.
 */
const FAILURE_SUMMARY_HEADER = /^failure summary\b/;

/** The `frost: ...` trailer. Frost's own words, never an action's output. */
const RUN_SUMMARY = /^frost:[ \t]/;

/**
 * A test action description, as frost writes it in `test_shards`:
 * `TEST {name}` or `TEST {name} (shard {index + 1}/{total})`.
 *
 * Case-sensitive, and the verb must be followed by whitespace and a non-space.
 * `TESTING foo` is a different verb, and a test's own stdout saying `test: ok`
 * is not a description at all — both must miss.
 */
const TEST_DESC = /^TEST[ \t]+(\S.*)$/;

/**
 * The shard suffix inside a description: `split (shard 1/3)`.
 *
 * The name is `\S+` because a target name is `[A-Za-z0-9_-]+` and a package
 * path is a directory path (`manifest.rs`), so a label never contains a space.
 * That is what makes the trailing `(shard n/m)` unambiguous rather than
 * something that might have come from the middle of a name.
 */
const SHARD_IN_DESC = /^(\S+)[ \t]+\(shard[ \t]+(\d+)\/(\d+)\)$/;

/** One framing line's worth of observation, before duplicates are resolved. */
interface Observation {
  outcome: TestOutcome;
  actionId: string;
  label: string;
  shard?: { index: number; total: number };
  /** Raw lines frost printed under the framing line, in order. */
  lines: string[];
}

/**
 * Turn an action description from a progress line into the action id frost
 * would use for it, or `undefined` when the description is not a test.
 *
 * Exported because of the one conversion in this module that is genuinely easy
 * to get wrong, and impossible to notice when you do: **the displayed shard
 * number is 1-based and the action id is 0-based**. `test_shards` builds the id
 * as `test:{name}#{index}/{total}` and the description as
 * `TEST {name} (shard {index + 1}/{total})` from the same `index`, so going the
 * other way has to subtract one. Off by one here does not crash and does not
 * look wrong: it just reports every shard's result against its neighbour, and
 * for the last shard against an id that does not exist.
 *
 * Non-test descriptions (`CC foo.c (//x:y)`, `LINK //x:y`, `AR libcore.a`)
 * return `undefined` rather than throwing, so a caller can push every progress
 * line of a mixed build through this function unfiltered.
 *
 * The candidate id is validated by round-tripping it through
 * `parseTestActionId` rather than by a second set of range checks here. That
 * keeps one definition of a well-formed id in the codebase, and it means a
 * nonsense display number cannot escape: `(shard 0/3)` would become `#-1/3`,
 * `(shard 4/3)` an index past its own total, and both are rejected there.
 *
 * A `(shard 1/1)` is refused separately, because `parseTestActionId` would
 * accept `#0/1` as well-formed. Frost never prints it — `test_shards` returns
 * the *unsuffixed* id and description for `total <= 1` — so accepting it would
 * manufacture an endpoint that resolves to nothing.
 */
export function progressDescriptionToActionId(desc: string): string | undefined {
  const verb = TEST_DESC.exec(desc.replace(/[ \t]+$/, ''));
  if (verb === null) {
    return undefined;
  }
  const rest = verb[1] as string;

  const shard = SHARD_IN_DESC.exec(rest);
  let actionId: string;
  if (shard === null) {
    actionId = `test:${rest}`;
  } else {
    const total = Number.parseInt(shard[3] as string, 10);
    if (total < 2) {
      return undefined;
    }
    // The subtraction. Everything else in this function exists to make sure
    // this line is reached with the right operands.
    const index = Number.parseInt(shard[2] as string, 10) - 1;
    actionId = `test:${shard[1] as string}#${index}/${total}`;
  }
  return parseTestActionId(actionId) === undefined ? undefined : actionId;
}

/**
 * Read one `frost test` run's output into an outcome per test action.
 *
 * Where each verdict comes from, and why:
 *
 * **Failed** comes from the `failure summary (first N):` block, which names
 * action ids outright and so needs no description-to-id guessing. Entries are
 * `  <action-id>: <first line of detail>` and are split on the FIRST `": "`,
 * because an action id contains colons of its own — `compile://core:core:
 * core/src/core.c` — while nothing frost puts in an id has a colon followed by
 * a space. Ids that `parseTestActionId` does not recognise are dropped: a
 * `frost test` whose compile step failed lists that compile action in the same
 * block, and it is not a test result.
 *
 * The block ends at the first line that is not indented, which in a test run is
 * the `tests:` line. Getting that boundary wrong is not a small error in either
 * direction — carrying on swallows the summary line and turns it into a bogus
 * action id, stopping early loses real failures.
 *
 * `FAILED: <desc>` lines are read as failures too, which the failure-summary
 * block alone would not give: that block is capped at the first ten. Past the
 * cap the ids exist only in the `FAILED:` lines, and without them the eleventh
 * failing test would simply vanish from the results while the `tests:` line
 * still counted it.
 *
 * **Passed** comes from `[n/m] TEST ...` progress lines whose id is not already
 * failed, via `progressDescriptionToActionId`. This is safe precisely because
 * `run_plain` prints that line only for an executed action; a failure gets
 * `FAILED:` instead, so no failing test can arrive here wearing a progress
 * line. The failed set still wins on a collision, because reporting a failure
 * as a pass is the one error that lets a broken build look finished.
 *
 * **Cached** is a count and nothing more. See `TestRunResult.cachedCount`.
 *
 * **Skipped** is not reported at all: frost prints nothing for a test it
 * skipped because a dependency failed, yet counts it in the `failed` number of
 * the `tests:` line. So `summary.failed` can legitimately exceed the number of
 * `failed` results, and a caller comparing the two should treat the difference
 * as "not run", not as a parse bug.
 *
 * Order is deterministic: failures in the order the failure summary reported
 * them (then any the cap dropped, in the order their `FAILED:` lines appeared),
 * then passes in progress order. Progress order is completion order, which is
 * scheduler-dependent — shard 3 finishing first is normal — so a caller that
 * needs a stable display order should sort by `actionId`, not rely on this one.
 * Results are deduplicated by `actionId`, which matters because one run names a
 * failing action twice: once on its `FAILED:` line and again in the summary.
 *
 * This parses ONE run. A buffer holding several — a `frost watch` session, say
 * — keeps the first verdict per action while `parseTestSummary` keeps the last
 * summary, so callers should slice per run rather than feed the whole buffer.
 */
export function parseTestRun(text: string): TestRunResult {
  const observations: Observation[] = [];
  const summaryEntries: Array<{ actionId: string; detail?: string }> = [];
  // Where the lines under the current framing line are collected, or undefined
  // when the last framing line was not an attributable test action. Leaving it
  // undefined is what stops a compiler's diagnostics from being filed as a
  // test's output.
  let sink: string[] | undefined;
  let inFailureSummary = false;

  // All three terminators: the output may have come from a Windows toolchain
  // even when the extension is not running on Windows.
  for (const line of text.split(/\r\n|\n|\r/)) {
    if (inFailureSummary) {
      const entry = failureSummaryEntry(line);
      if (entry !== undefined) {
        summaryEntries.push(entry);
        continue;
      }
      // First unindented line closes the block. Fall through, so that line is
      // still read as ordinary output rather than consumed.
      inFailureSummary = false;
    }

    if (FAILURE_SUMMARY_HEADER.test(line)) {
      sink = undefined;
      inFailureSummary = true;
      continue;
    }

    if (RUN_SUMMARY.test(line)) {
      sink = undefined;
      continue;
    }

    const failed = FAILED.exec(line);
    if (failed !== null) {
      sink = observe(observations, 'failed', failed[1] as string);
      continue;
    }

    const progress = PROGRESS.exec(line);
    if (progress !== null) {
      sink = observe(observations, 'passed', progress[1] as string);
      continue;
    }

    sink?.push(line);
  }

  // Failures first, keyed by id so the two places that name them collapse. A
  // `Map` set on a key it already has keeps that key's position, which is what
  // lets the richer `FAILED:` detail replace the summary's single line without
  // reordering the results.
  const failures = new Map<string, TestActionResult>();
  for (const entry of summaryEntries) {
    const parsed = parseTestActionId(entry.actionId);
    if (parsed === undefined || failures.has(entry.actionId)) {
      continue;
    }
    failures.set(entry.actionId, toResult(entry.actionId, parsed, 'failed', entry.detail));
  }
  for (const observation of observations) {
    if (observation.outcome !== 'failed') {
      continue;
    }
    const detail =
      joinDetail(observation.lines) ?? failures.get(observation.actionId)?.detail;
    failures.set(
      observation.actionId,
      toResult(observation.actionId, observation, 'failed', detail),
    );
  }

  const passes = new Map<string, TestActionResult>();
  for (const observation of observations) {
    if (
      observation.outcome !== 'passed' ||
      failures.has(observation.actionId) ||
      passes.has(observation.actionId)
    ) {
      continue;
    }
    passes.set(
      observation.actionId,
      toResult(
        observation.actionId,
        observation,
        'passed',
        joinDetail(observation.lines),
      ),
    );
  }

  const summary = parseTestSummary(text);
  const result: TestRunResult = {
    results: [...failures.values(), ...passes.values()],
    cachedCount: summary?.cached ?? 0,
  };
  if (summary !== undefined) {
    result.summary = summary;
  }
  return result;
}

/**
 * Record a framing line's action, and hand back the array its output belongs
 * in — or `undefined` when the description is not a test.
 *
 * Returning the sink from the same call that decides whether the action counts
 * is deliberate: it makes "not a test action" and "do not attribute the lines
 * that follow" a single decision, which cannot then drift apart.
 */
function observe(
  into: Observation[],
  outcome: TestOutcome,
  description: string,
): string[] | undefined {
  const actionId = progressDescriptionToActionId(description);
  if (actionId === undefined) {
    return undefined;
  }
  const parsed = parseTestActionId(actionId);
  if (parsed === undefined) {
    // Unreachable: `progressDescriptionToActionId` validates through the same
    // function. Written as a guard anyway so a future change to either one
    // cannot produce a result with a label nobody parsed.
    return undefined;
  }
  const observation: Observation = {
    outcome,
    actionId,
    label: parsed.label,
    lines: [],
  };
  if (parsed.shard !== undefined) {
    observation.shard = parsed.shard;
  }
  into.push(observation);
  return observation.lines;
}

/**
 * Split one entry of the failure summary into its action id and its detail.
 *
 * The separator is the first `": "` for the reason spelled out on
 * `parseTestRun`: ids contain colons, so neither the first `:` nor the last one
 * is the boundary. `diagnostics.ts` splits the same block the same way for its
 * own purposes — the two are intentionally independent, since one wants the
 * diagnostics under a failure and this one wants the verdict.
 *
 * A failure with empty detail prints as `  <id>: ` with a trailing space, so the
 * search runs against the untrimmed line and still finds its separator.
 */
function failureSummaryEntry(
  line: string,
): { actionId: string; detail?: string } | undefined {
  if (!line.startsWith('  ')) {
    return undefined;
  }
  const body = line.slice(2);
  const separator = body.indexOf(': ');
  if (separator === -1) {
    const actionId = body.replace(/:$/, '');
    return actionId === '' ? undefined : { actionId };
  }
  const actionId = body.slice(0, separator);
  if (actionId === '') {
    return undefined;
  }
  const detail = body.slice(separator + 2).replace(/[ \t]+$/, '');
  return detail === '' ? { actionId } : { actionId, detail };
}

/**
 * The captured lines as one string, or `undefined` when there were none.
 *
 * Blank lines at either end are dropped but interior ones are kept: a test
 * runner's report is often paragraphed, and squeezing it would make the detail
 * harder to read in the exact case where someone is reading it. `undefined`
 * rather than `''` so the field can be omitted from the result entirely — an
 * action that printed nothing is a different fact from one that printed
 * whitespace.
 */
function joinDetail(lines: readonly string[]): string | undefined {
  let start = 0;
  let end = lines.length;
  while (start < end && (lines[start] as string).trim() === '') {
    start += 1;
  }
  while (end > start && (lines[end - 1] as string).trim() === '') {
    end -= 1;
  }
  return start === end ? undefined : lines.slice(start, end).join('\n');
}

/** Assemble a result, omitting optional fields rather than setting `undefined`. */
function toResult(
  actionId: string,
  parsed: { label: string; shard?: { index: number; total: number } },
  outcome: TestOutcome,
  detail: string | undefined,
): TestActionResult {
  const result: TestActionResult = { actionId, label: parsed.label, outcome };
  if (parsed.shard !== undefined) {
    result.shard = parsed.shard;
  }
  if (detail !== undefined) {
    result.detail = detail;
  }
  return result;
}
