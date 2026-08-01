// The Test Explorer.
//
// The model — which targets are tests, how a sharded one decomposes, what a run
// reported per shard — is computed and tested in `frost/tests.ts` and
// `frost/testrun.ts`. This is the binding, and it is deliberately thin.

import * as vscode from 'vscode';

import { buildTestItems } from './frost/tests';
import { parseTestRun } from './frost/testrun';
import type { TestItem as FrostTestItem } from './frost/types';
import type { FrostRunner } from './runner';
import type { TargetIndex } from './targets-provider';

export class FrostTestExplorer {
  private readonly items = new Map<string, { folder: vscode.WorkspaceFolder; label: string }>();

  constructor(
    private readonly controller: vscode.TestController,
    private readonly targets: TargetIndex,
    private readonly runner: FrostRunner,
  ) {
    controller.resolveHandler = async () => {
      await this.discover();
    };
    controller.createRunProfile(
      'Run',
      vscode.TestRunProfileKind.Run,
      (request, token) => this.run(request, token),
      true,
    );
  }

  /** Populate the tree from the workspace's test targets. */
  async discover(): Promise<void> {
    this.controller.items.replace([]);
    this.items.clear();
    for (const folder of vscode.workspace.workspaceFolders ?? []) {
      const targets = await this.targets.list(folder);
      for (const test of buildTestItems(targets)) {
        this.controller.items.add(this.itemFor(folder, test));
      }
    }
  }

  private itemFor(
    folder: vscode.WorkspaceFolder,
    test: FrostTestItem,
  ): vscode.TestItem {
    const item = this.controller.createTestItem(test.label, test.label);
    this.items.set(test.label, { folder, label: test.label });
    // Shards are children rather than separate top-level entries: they are one
    // test split for scheduling, and presenting them as peers would tell the
    // reader the workspace has three tests where the manifest declares one.
    if (test.shards.length > 1) {
      for (const shard of test.shards) {
        const child = this.controller.createTestItem(
          shard.actionId,
          `shard ${(shard.index ?? 0) + 1}/${shard.total}`,
        );
        item.children.add(child);
      }
    }
    return item;
  }

  private async run(
    request: vscode.TestRunRequest,
    token: vscode.CancellationToken,
  ): Promise<void> {
    const run = this.controller.createTestRun(request);
    const queue: vscode.TestItem[] = [];
    if (request.include) {
      queue.push(...request.include);
    } else {
      this.controller.items.forEach((item) => queue.push(item));
    }
    for (const item of queue) {
      if (token.isCancellationRequested) {
        break;
      }
      const known = this.items.get(item.id);
      if (!known) {
        continue;
      }
      run.started(item);
      const outcome = await this.runner.run(known.folder, 'test', [known.label]);
      if (!outcome) {
        run.errored(item, new vscode.TestMessage('frost could not be started'));
        continue;
      }
      const parsed = parseTestRun(outcome.output);
      const failures = parsed.results.filter(
        (result) => result.label === known.label && result.outcome === 'failed',
      );
      if (failures.length > 0) {
        run.failed(
          item,
          new vscode.TestMessage(
            failures.map((failure) => failure.detail ?? failure.actionId).join('\n'),
          ),
        );
      } else if (outcome.code === 0) {
        run.passed(item);
      } else {
        // Non-zero without an attributable test failure means the run failed
        // before the test did — a compile error in a dependency, usually.
        run.errored(
          item,
          new vscode.TestMessage(
            parsed.summary
              ? `run failed: ${JSON.stringify(parsed.summary)}`
              : 'run failed before the test executed',
          ),
        );
      }
      // Shard children mirror the parent: per-shard attribution exists in the
      // parsed result, but a cached shard prints nothing, so reporting a shard
      // as passed on the strength of silence would be a claim frost did not
      // make.
      item.children.forEach((child) => {
        const shard = parsed.results.find((result) => result.actionId === child.id);
        if (!shard) {
          run.skipped(child);
        } else if (shard.outcome === 'failed') {
          run.failed(child, new vscode.TestMessage(shard.detail ?? child.id));
        } else {
          run.passed(child);
        }
      });
    }
    run.end();
  }
}
