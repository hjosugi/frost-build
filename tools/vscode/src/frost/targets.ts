// The target list behind the sidebar tree. Pure by design, like the rest of
// `src/frost/`: it is handed the stdout that `cli.queryLabelKind` captured and
// returns plain data, so the whole tree can be exercised in CI without a VS
// Code download or a built frost binary.
//
// `frost query <fn> --output label-kind` is a contract surface (see
// docs/28_compatibility_contract.md), which is what makes parsing it
// reasonable at all. The parser is still written defensively, because
// "contract" promises the shape of a line, not the set of values that can
// appear in it.

import type { LabeledTarget, TargetKind } from './types';

/**
 * Every kind frost can print, as a runtime lookup table.
 *
 * Spelled as a `Record<TargetKind, true>` rather than a string array so the
 * compiler enforces both directions: a kind added to `TargetKind` and not
 * added here fails to compile (missing property), and a kind here that
 * `TargetKind` does not have fails too (excess property). A parser that
 * decides what is valid should not be able to drift from the type that
 * describes what is valid.
 */
const TARGET_KINDS: Record<TargetKind, true> = {
  cc_binary: true,
  cc_library: true,
  cc_test: true,
  genrule: true,
  test: true,
  kofun_binary: true,
  command: true,
};

/**
 * `<kind> target <label>`, the exact shape frost prints.
 *
 * Anchored and whitespace-free on both ends rather than a `' target '` split,
 * so a line that merely *contains* the separator — a stray log line, a shell
 * that echoed the command — cannot be mistaken for a target. Neither kinds nor
 * labels contain spaces, so requiring three fields costs nothing.
 */
const LABEL_KIND_LINE = /^(\S+) target (\S+)$/;

/** What `buildTargetTree` shows for the workspace root, which has no segment. */
const ROOT_DISPLAY_NAME = '//';

/**
 * Narrow a kind string frost printed to the kinds this extension knows.
 *
 * `Object.hasOwn` rather than `value in TARGET_KINDS`, because `in` also finds
 * inherited members: a target somehow named `constructor` or `toString` would
 * otherwise be accepted as a valid kind.
 */
function isTargetKind(value: string): value is TargetKind {
  return Object.hasOwn(TARGET_KINDS, value);
}

/**
 * Parse `frost query <fn> --output label-kind` into targets, in input order.
 *
 * Order is preserved rather than normalized: frost already emits query results
 * in a deterministic order, and callers that want a different one (the tree
 * sorts by name) should say so explicitly instead of inheriting whatever this
 * function happened to do.
 *
 * Unparseable lines are skipped, not thrown on. Two things can produce one:
 * frost itself prints `unknown target <name>` for a name it has no target
 * record for, and a newer frost may print a kind this extension predates. In
 * both cases a sidebar that renders every target it understands is more useful
 * than one that renders nothing because of a single row. The skip is
 * deliberately not a fallback to some default kind — accepting an unrecognized
 * kind as a `TargetKind` would push the lie past the type system and into the
 * commands built from it (a "Run" on something that is not runnable).
 */
export function parseLabelKind(text: string): LabeledTarget[] {
  const targets: LabeledTarget[] = [];
  for (const rawLine of text.split('\n')) {
    // Full trim, not `trimEnd`: this handles the `\r` of CRLF stdout on
    // Windows, which would otherwise ride along on the label and turn every
    // label the extension passes back to frost into an unknown target.
    const line = rawLine.trim();
    if (line === '') {
      continue;
    }
    const match = LABEL_KIND_LINE.exec(line);
    if (match === null) {
      continue;
    }
    // Both groups always participate in a match; the `?? ''` is only so this
    // keeps compiling if the project turns on `noUncheckedIndexedAccess`.
    const kind = match[1] ?? '';
    const label = match[2] ?? '';
    if (!isTargetKind(kind)) {
      continue;
    }
    const { packagePath, name } = parseLabel(label);
    if (name === '') {
      // `//pkg:` names nothing. Keeping it would put a blank row in the tree
      // and a label frost cannot resolve behind it.
      continue;
    }
    targets.push({ kind, label, packagePath, name });
  }
  return targets;
}

/**
 * Split a frost label into the package that declares it and its name.
 *
 * Three shapes reach this: `//apps/cli:cli` (a package target), `gen_version`
 * (a root target, which frost prints bare — `resolve_label` in
 * frostbuild-core rewrites `//:name` to `name` before it ever reaches the
 * graph), and `//:root_target`, which frost does not print today but which a
 * caller can reasonably hand us from a manifest.
 *
 * A `//`-prefixed label with no `:` is read as a bare name, *not* as Bazel's
 * `//pkg` → `//pkg:pkg` shorthand: frost does not apply that shorthand either,
 * so inventing a package here would invent a grouping the build tool does not
 * agree with. The caller's `label` is always preserved verbatim by
 * `parseLabelKind`, so this only ever affects where a node is displayed, never
 * what gets passed back to frost.
 */
export function parseLabel(label: string): {
  packagePath: string;
  name: string;
} {
  const body = label.startsWith('//') ? label.slice(2) : label;
  const separator = body.indexOf(':');
  if (separator === -1) {
    return { packagePath: '', name: body };
  }
  return {
    packagePath: body.slice(0, separator),
    name: body.slice(separator + 1),
  };
}

/** One package in the sidebar tree, with the targets it declares. */
export interface TargetTreeNode {
  /** Full package path, `''` for the root package. */
  packagePath: string;
  /** Last path segment; `'//'` for the root package, which has no segment. */
  displayName: string;
  /** Targets declared by this package, sorted by name. */
  targets: LabeledTarget[];
  /** Sub-packages, sorted by display name. */
  children: TargetTreeNode[];
}

/**
 * Group targets into the nested package tree the sidebar renders.
 *
 * Packages with no targets of their own are still materialized as nodes:
 * `//a/b/c:x` alone produces a → b → c, because a tree cannot show `c` without
 * showing the path to it. Such a node is distinguishable from a package that
 * genuinely declares nothing by its empty `targets`, which is what lets the
 * view collapse or grey it.
 *
 * The result is a function of the target set alone, not of the order it
 * arrived in — the same workspace queried two ways must produce the same tree,
 * or the sidebar reshuffles between refreshes.
 */
export function buildTargetTree(targets: LabeledTarget[]): TargetTreeNode {
  const root: TargetTreeNode = {
    packagePath: '',
    displayName: ROOT_DISPLAY_NAME,
    targets: [],
    children: [],
  };
  // Keyed by package path so an intermediate package is created once no matter
  // how many descendants ask for it, which also keeps this linear in the
  // number of path segments rather than quadratic in the number of targets.
  const byPath = new Map<string, TargetTreeNode>([['', root]]);

  for (const target of targets) {
    packageNode(target.packagePath, root, byPath).targets.push(target);
  }
  sortNode(root);
  return root;
}

/** Find or create the node for `packagePath`, creating any missing ancestors. */
function packageNode(
  packagePath: string,
  root: TargetTreeNode,
  byPath: Map<string, TargetTreeNode>,
): TargetTreeNode {
  const cached = byPath.get(packagePath);
  if (cached !== undefined) {
    return cached;
  }
  // Empty segments are dropped rather than honored: `a//b` or a trailing slash
  // would otherwise create a node whose display name is the empty string,
  // which renders as an unclickable blank row.
  const segments = packagePath.split('/').filter((segment) => segment !== '');
  let node = root;
  let prefix = '';
  for (const segment of segments) {
    prefix = prefix === '' ? segment : `${prefix}/${segment}`;
    let child = byPath.get(prefix);
    if (child === undefined) {
      child = {
        packagePath: prefix,
        displayName: segment,
        targets: [],
        children: [],
      };
      byPath.set(prefix, child);
      node.children.push(child);
    }
    node = child;
  }
  // Also cache under the spelling the caller used, so a denormalized path
  // resolves to the same node next time instead of walking again.
  byPath.set(packagePath, node);
  return node;
}

/** Sort a node's targets and children, then its children's, in place. */
function sortNode(node: TargetTreeNode): void {
  node.targets.sort((left, right) => compare(left.name, right.name));
  node.children.sort((left, right) =>
    compare(left.displayName, right.displayName),
  );
  for (const child of node.children) {
    sortNode(child);
  }
}

/**
 * Order two strings by code unit.
 *
 * Not `localeCompare`: its collation depends on the host's ICU data and the
 * active locale, so the same workspace would order differently on two
 * developers' machines and, more annoyingly, between a test run and CI. Target
 * names are ASCII identifiers, where code-unit order is also the obvious one.
 * Ties keep input order, since `Array.prototype.sort` is stable.
 */
function compare(left: string, right: string): number {
  if (left < right) {
    return -1;
  }
  return left > right ? 1 : 0;
}

/**
 * Whether a kind is run by `frost test`.
 *
 * These are exactly the two kinds frost collects for a test run ("workspace
 * declares no cc_test or test targets" is its error when neither exists), so
 * this is the predicate behind the Test Explorer's population.
 */
export function isTestKind(kind: TargetKind): boolean {
  return kind === 'test' || kind === 'cc_test';
}

/**
 * Whether a kind produces a binary the editor can offer to launch directly.
 *
 * Deliberately disjoint from `isTestKind`, and deliberately excludes
 * `cc_test` even though it does link a binary into `bin_dir`: a test is
 * launched through `frost test` so that sharding, test environment and result
 * reporting apply, and offering a bare "Run" would quietly bypass all three.
 * `genrule` and `command` run a command *during* the build and leave nothing
 * behind to launch afterwards; `cc_library` produces an archive, not an entry
 * point.
 */
export function isRunnableKind(kind: TargetKind): boolean {
  return kind === 'cc_binary' || kind === 'kofun_binary';
}
