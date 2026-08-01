// Test discovery, and reading what a `frost test` run reported.
//
// Pure text in, plain data out. `cli.ts` is the only module that spawns frost;
// keeping this one free of `vscode` and of child processes is what lets it run
// under `node:test` in CI with no VS Code download and no built frost binary.
//
// Scope is deliberately two things: the `tests:` summary line, and the shape of
// a test action id. Compiler diagnostics are read elsewhere — folding them in
// here would tie test discovery to every toolchain frost can drive.

import type { LabeledTarget, TestItem, TestShard, TestSummary } from './types';

/**
 * The action-id prefix frost gives every test action.
 *
 * Mirrors `test_shards` in `crates/frostbuild-core/src/graph.rs`, which builds
 * `test:{name}` and `test:{name}#{index}/{total}`. The two spellings have to
 * stay byte-identical: the extension hands these ids back to frost as build
 * endpoints, so a wrong one is an unbuildable target, not a slow one.
 */
const TEST_ACTION_PREFIX = 'test:';

/**
 * The summary `frost test` prints: `tests: N passed, N failed, N cached`,
 * sometimes followed by ` (no affected tests)`.
 *
 * Not anchored to a line start, on purpose. The extension reads stdout and
 * stderr interleaved (`FrostRun.output`), so a progress fragment can land in
 * front of the summary on one physical line; anchoring would lose the summary
 * in exactly the noisy runs where it matters most. The separators are `[ \t]`
 * rather than `\s` so a match can never span a newline — that bound is what
 * makes the unanchored form safe, and it makes CRLF input a non-event.
 */
const TEST_SUMMARY_LINE =
  /tests:[ \t]+(\d+)[ \t]+passed,[ \t]+(\d+)[ \t]+failed,[ \t]+(\d+)[ \t]+cached/g;

/** A shard suffix, matched against the text after the final `#`. */
const SHARD_SUFFIX = /^(\d+)\/(\d+)$/;

/** What `parseTestActionId` recovers from an id frost printed. */
export interface ParsedTestActionId {
  /** Full target label, exactly as frost spells it: `//pkg:name` or `name`. */
  label: string;
  /** Present only for a sharded action; `index` is 0-based. */
  shard?: { index: number; total: number };
}

/**
 * Read the `tests:` summary out of captured `frost test` output.
 *
 * `undefined` when there is no such line. That is a different fact from "zero
 * tests ran": a plain `frost build` prints no summary at all, and the caller
 * should show no test count rather than a misleading `0 passed`.
 *
 * The last summary wins. One captured buffer can hold more than one — a task
 * that builds and then tests, or a watch session appending runs — and the most
 * recent is the one that describes the tree as it stands now.
 */
export function parseTestSummary(text: string): TestSummary | undefined {
  // `matchAll` iterates a clone of the pattern, so this module-level `/g`
  // literal carries no `lastIndex` across calls. The same regex driven by
  // `.exec()` would, and would silently skip matches on every other call.
  let last: RegExpMatchArray | undefined;
  for (const match of text.matchAll(TEST_SUMMARY_LINE)) {
    last = match;
  }
  if (last === undefined) {
    return undefined;
  }
  const [, passed, failed, cached] = last;
  return {
    passed: toCount(passed),
    failed: toCount(failed),
    cached: toCount(cached),
  };
}

/**
 * Split a test action id into the target label and, if present, its shard.
 *
 * `undefined` for anything that is not a test action, so a caller can filter a
 * mixed list of ids (`compile:...`, `link:...`) through this function directly.
 *
 * The parsing is fiddly for one reason: the label is embedded whole, and a
 * label contains both `:` and `/` (`//tools/vscode:unit_test`). So the prefix
 * is stripped by length rather than by finding a `:`, and the shard is split on
 * the LAST `#`. That last `#` is unambiguous because a target name must match
 * `[A-Za-z0-9_-]+` (`manifest.rs`) and a package path is a directory path —
 * neither can contain `#`, so the only `#` in a well-formed id is the one
 * sharding put there.
 *
 * A `#` whose suffix is not a well-formed `index/total` is rejected rather than
 * folded back into the label: since `#` cannot occur in a label, keeping it
 * would manufacture a label no target has, and the caller would run a build for
 * an endpoint that cannot resolve. Returning `undefined` makes it a skip.
 */
export function parseTestActionId(
  actionId: string,
): ParsedTestActionId | undefined {
  if (!actionId.startsWith(TEST_ACTION_PREFIX)) {
    return undefined;
  }
  const rest = actionId.slice(TEST_ACTION_PREFIX.length);
  if (rest === '') {
    return undefined;
  }

  const hash = rest.lastIndexOf('#');
  if (hash === -1) {
    return { label: rest };
  }

  const suffix = SHARD_SUFFIX.exec(rest.slice(hash + 1));
  if (suffix === null) {
    return undefined;
  }
  const index = toCount(suffix[1]);
  const total = toCount(suffix[2]);
  // `total < 1`, or an index outside its own total, is not something
  // `test_shards` can emit. Admitting one would put a phantom shard in the
  // test tree that no run can ever report on.
  if (total < 1 || index >= total) {
    return undefined;
  }
  const label = rest.slice(0, hash);
  if (label === '') {
    return undefined;
  }
  return { label, shard: { index, total } };
}

/**
 * Turn `frost query --output label-kind` results into runnable test items.
 *
 * Non-test kinds are dropped rather than rejected, because the natural queries
 * (`deps`, `rdeps`, a whole-workspace listing) return libraries and genrules
 * alongside the tests and the caller should not have to pre-filter.
 *
 * `shardCounts` is optional because the extension often cannot know the counts
 * up front: `shard_count` lives in `frost.toml`, and the label-kind output does
 * not carry it. Assuming unsharded is the safe default, and deliberately so —
 * running `test:NAME` against a target that is in fact sharded is a *miss* (the
 * endpoint does not resolve, frost says so, nothing runs) rather than a wrong
 * result, and the real ids get filled in by `mergeObservedShards` the first
 * time a run reports them. Assuming sharded would be the unsafe direction: it
 * would invent `#i/n` ids for unsharded targets and never converge.
 */
export function buildTestItems(
  targets: readonly LabeledTarget[],
  shardCounts?: ReadonlyMap<string, number>,
): TestItem[] {
  const items: TestItem[] = [];
  for (const target of targets) {
    const kind = testKindOf(target);
    if (kind === undefined) {
      continue;
    }
    items.push({
      label: target.label,
      kind,
      packagePath: target.packagePath,
      name: target.name,
      shards: shardsFor(target.label, shardCounts?.get(target.label)),
    });
  }
  items.sort((a, b) => compareLabels(a.label, b.label));
  return items;
}

/**
 * Replace an item's assumed shards with the ones a run actually reported.
 *
 * This is the correction half of `buildTestItems`' unsharded assumption: the
 * ids in real output are the ground truth about how a target is split, so a
 * single assumed `test:NAME` becomes the N `test:NAME#i/N` frost named, and a
 * target whose `shard_count` was removed collapses back the other way.
 *
 * Ids for other labels are ignored, so the caller can pass every id from a run
 * without bucketing them first. When nothing matches, the item is returned by
 * identity — a run that touched other targets says nothing about this one, and
 * the identity lets a caller skip rebuilding that part of the tree.
 *
 * Callers should pass the ids from a whole run, not just the failures: this
 * takes the observed set literally, so feeding it only `test:NAME#1/3` out of a
 * failure summary would narrow a 3-shard item to that one shard.
 */
export function mergeObservedShards(
  item: TestItem,
  observedActionIds: readonly string[],
): TestItem {
  // Keyed by action id because one run names the same shard more than once —
  // progress prints it, and the failure summary prints it again. A plain array
  // would show the failing shard twice in the tree.
  const observed = new Map<string, TestShard>();
  for (const actionId of observedActionIds) {
    const parsed = parseTestActionId(actionId);
    if (parsed === undefined || parsed.label !== item.label) {
      continue;
    }
    observed.set(
      actionId,
      parsed.shard === undefined
        ? { actionId, total: 1 }
        : { actionId, index: parsed.shard.index, total: parsed.shard.total },
    );
  }
  if (observed.size === 0) {
    return item;
  }
  // Frost reports shards in completion order, which is scheduler-dependent and
  // therefore different run to run; the tree must not reshuffle underneath the
  // user. An unsharded entry has no index and sorts first, so the order stays
  // total even in the mixed case that only a mid-run config change could
  // produce. A fresh array leaves the caller's item untouched.
  const shards = [...observed.values()].sort(
    (a, b) => (a.index ?? -1) - (b.index ?? -1),
  );
  return { ...item, shards };
}

/** Digits to a number, written total so it survives `noUncheckedIndexedAccess`. */
function toCount(digits: string | undefined): number {
  return digits === undefined ? 0 : Number.parseInt(digits, 10);
}

/** The target's kind if it is a test kind, narrowed to what `TestItem` accepts. */
function testKindOf(target: LabeledTarget): 'test' | 'cc_test' | undefined {
  return target.kind === 'test' || target.kind === 'cc_test'
    ? target.kind
    : undefined;
}

/**
 * The shard list to assume for a label.
 *
 * `count <= 1` means unsharded and produces the bare `test:NAME`, matching
 * `test_shards`, which returns the unsuffixed id for `total <= 1`. Frost never
 * emits `#0/1`, so producing one here would be an id that resolves to nothing.
 * A non-integer or absent count falls into the same branch: an unusable count
 * should degrade to the safe assumption, not throw at tree-building time.
 */
function shardsFor(label: string, count: number | undefined): TestShard[] {
  const base = `${TEST_ACTION_PREFIX}${label}`;
  if (count === undefined || !Number.isInteger(count) || count <= 1) {
    return [{ actionId: base, total: 1 }];
  }
  const shards: TestShard[] = [];
  for (let index = 0; index < count; index += 1) {
    shards.push({ actionId: `${base}#${index}/${count}`, index, total: count });
  }
  return shards;
}

/**
 * Order labels by code unit, not `localeCompare`.
 *
 * The test tree has to come out the same on every machine; `localeCompare`
 * depends on the host's ICU locale, so it would order `//a` against `//A`
 * differently for two developers looking at the same workspace.
 */
function compareLabels(a: string, b: string): number {
  if (a < b) {
    return -1;
  }
  return a > b ? 1 : 0;
}
