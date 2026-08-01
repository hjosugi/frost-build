// Target enumeration and its cache.
//
// Lifted out of `extension.ts` because three separate consumers need it — the
// task provider, the quick picks and the tree view — and a module-level `Map`
// reachable from all of them is how the stale-cache bug got in.

import * as vscode from 'vscode';

import { queryLabelKind, queryTargets } from './frost/cli';
import { parseLabelKind } from './frost/targets';
import type { LabeledTarget } from './frost/types';
import { cliOptions, readSettings } from './workspace';

export class TargetIndex {
  private readonly cache = new Map<string, LabeledTarget[]>();

  /** Drop everything. Bound to the refresh command and to setting changes. */
  clear(): void {
    this.cache.clear();
  }

  /**
   * Every target in a folder, with its kind.
   *
   * Two invocations rather than one because frost has no primitive for "list
   * the universe": `query deps`/`rdeps` both need a starting target and `pick`
   * needs fzf. `graph --dot` supplies the topology, its roots are derived, and
   * `query deps <root> --output label-kind` supplies the kinds — `label-kind`
   * rather than `--json` because the JSON form carries only names, and a tree
   * that cannot tell a test from a library is not worth showing.
   *
   * Roots are usually one or two, so this is a couple of invocations, cached.
   * A `frost query targets` answering it in one call would replace all of this.
   */
  async list(folder: vscode.WorkspaceFolder): Promise<LabeledTarget[]> {
    const settings = readSettings(folder.uri);
    // The key covers what the answer depends on, not just where it came from.
    // Keying on the folder alone means correcting `frostbuild.binaryPath` — the
    // exact thing someone does after seeing an empty target list — leaves the
    // empty list in place until they find the refresh command.
    const key = [
      folder.uri.toString(),
      settings.binaryPath,
      settings.profile,
      settings.platform,
    ].join(' ');
    const cached = this.cache.get(key);
    if (cached) {
      return cached;
    }
    const options = cliOptions(folder, settings);
    // `frost query` has no --profile/--platform of its own; the target set is
    // a property of the manifest, not of the configuration it is built for.
    // The cache key still covers both, so a profile change refreshes anyway.
    const text = await queryTargets([], options).catch(() => undefined);
    // A folder that is not a frost workspace produces nothing here, which is
    // the answer rather than an error: a multi-root window may hold both. It is
    // deliberately not cached — a frost that has not been built yet fails the
    // same way, and remembering that would outlive the reason for it.
    if (text === undefined) {
      return [];
    }
    // frost prints these sorted, but the comparison is its own; sorting here
    // keeps the tree's order a property of the tree rather than of two
    // languages agreeing about how to order strings.
    const targets = parseLabelKind(text).sort((a, b) =>
      a.label < b.label ? -1 : a.label > b.label ? 1 : 0,
    );
    this.cache.set(key, targets);
    return targets;
  }

  /** Targets that declare `relative` among their action inputs. */
  async owners(
    folder: vscode.WorkspaceFolder,
    relative: string,
  ): Promise<LabeledTarget[]> {
    const settings = readSettings(folder.uri);
    const text = await queryLabelKind(
      ['owners', relative],
      cliOptions(folder, settings),
    ).catch(() => '');
    return parseLabelKind(text);
  }
}
