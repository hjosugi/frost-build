// Command handlers.
//
// Separated from activation so `extension.ts` reads as a list of what exists
// rather than as the implementation of all of it.

import * as vscode from 'vscode';

import { isTestKind } from './frost/targets';
import type { FrostRunner } from './runner';
import type { TargetIndex } from './targets-provider';
import { frostFolder, relativePath } from './workspace';

export interface CommandDeps {
  targets: TargetIndex;
  runner: FrostRunner;
  refreshViews: () => void;
}

/** Ask which target, listing kinds so the choice is informed. */
export async function pickTarget(
  targets: TargetIndex,
  folder: vscode.WorkspaceFolder,
  only: 'all' | 'tests',
): Promise<string | undefined> {
  const all = await targets.list(folder);
  const choices = all.filter(
    (target) => only === 'all' || isTestKind(target.kind),
  );
  if (choices.length === 0) {
    void vscode.window.showWarningMessage(
      `FrostBuild: no ${only === 'tests' ? 'test ' : ''}targets found in ${folder.name}.`,
    );
    return undefined;
  }
  const picked = await vscode.window.showQuickPick(
    choices.map((target) => ({ label: target.label, description: target.kind })),
    { placeHolder: only === 'tests' ? 'frost test' : 'frost build' },
  );
  return picked?.label;
}

export function registerCommands(deps: CommandDeps): vscode.Disposable[] {
  const { targets, runner, refreshViews } = deps;

  const pickAndRun = async (command: 'build' | 'test'): Promise<void> => {
    const folder = frostFolder();
    if (!folder) {
      void vscode.window.showWarningMessage(
        'FrostBuild: no workspace folder is open.',
      );
      return;
    }
    const target = await pickTarget(
      targets,
      folder,
      command === 'test' ? 'tests' : 'all',
    );
    if (target) {
      await runner.run(folder, command, [target]);
    }
  };

  const buildFileOwners = async (uri?: vscode.Uri): Promise<void> => {
    const resource = uri ?? vscode.window.activeTextEditor?.document.uri;
    if (!resource) {
      return;
    }
    const folder = frostFolder(resource);
    if (!folder) {
      return;
    }
    const relative = relativePath(folder, resource);
    if (!relative) {
      return;
    }
    const owners = await targets.owners(folder, relative);
    if (owners.length === 0) {
      // Not an error: a header frost only learns about from a depfile has no
      // owning target in the configuration. Say which, so the empty answer does
      // not look like a broken command.
      void vscode.window.showInformationMessage(
        `FrostBuild: no target declares ${relative} among its inputs.`,
      );
      return;
    }
    await runner.run(
      folder,
      'build',
      owners.map((owner) => owner.label),
    );
  };

  return [
    vscode.commands.registerCommand('frost.build', () => pickAndRun('build')),
    vscode.commands.registerCommand('frost.test', () => pickAndRun('test')),
    vscode.commands.registerCommand('frost.buildFileOwners', buildFileOwners),
    vscode.commands.registerCommand('frost.refreshTargets', () => {
      targets.clear();
      refreshViews();
    }),
    // Invoked from a tree row, which already knows its folder and label, so it
    // skips the quick pick entirely.
    vscode.commands.registerCommand(
      'frost.buildTarget',
      async (label: string, folder: vscode.WorkspaceFolder) => {
        await runner.run(folder, 'build', [label]);
      },
    ),
    vscode.commands.registerCommand(
      'frost.testTarget',
      async (label: string, folder: vscode.WorkspaceFolder) => {
        await runner.run(folder, 'test', [label]);
      },
    ),
    vscode.commands.registerCommand(
      'frost.debugTarget',
      async (label: string, folder: vscode.WorkspaceFolder) => {
        // Everything about how to debug is resolved by the debug provider, so
        // the tree row only has to say which target.
        await vscode.debug.startDebugging(folder, {
          type: 'frost',
          request: 'launch',
          name: `Frost: debug ${label}`,
          target: label,
        });
      },
    ),
  ];
}
