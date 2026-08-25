// Activation. Everything here is wiring: what exists, and what it is given.
// The behaviour lives in the modules below, and the parsing beneath those in
// `src/frost/`, which never imports this module's dependency.

import * as vscode from 'vscode';

import { pickTarget, registerCommands } from './commands';
import { DaemonStatusMonitor } from './daemon-status';
import { DEBUG_TYPE, FrostDebugConfigurationProvider } from './debug';
import { isTestKind } from './frost/targets';
import { FrostRunner } from './runner';
import { createStatusReporter } from './status';
import { FrostTaskProvider, TASK_TYPE } from './tasks';
import { TargetIndex } from './targets-provider';
import { FrostTestExplorer } from './tests-view';
import { TargetTreeProvider } from './tree';
import { frostFolder, readSettings } from './workspace';

export function activate(context: vscode.ExtensionContext): void {
  // Gates the editor context menu. Activation is `workspaceContains:frost.toml`,
  // so this is only ever true in a window that has one; without it the
  // "Build File Owners" entry appears on every file in every window that
  // happens to have activated the extension, which is a menu item that
  // usually cannot work.
  void vscode.commands.executeCommand('setContext', 'frostbuild.active', true);
  const output = vscode.window.createOutputChannel('FrostBuild');
  const diagnostics = vscode.languages.createDiagnosticCollection('frost');
  const status = createStatusReporter();
  const daemonStatus = new DaemonStatusMonitor(status);
  context.subscriptions.push(output, diagnostics, status, daemonStatus);

  const targets = new TargetIndex();
  const runner = new FrostRunner(output, diagnostics, status);
  const tree = new TargetTreeProvider(targets);
  const controller = vscode.tests.createTestController('frost', 'FrostBuild');
  const explorer = new FrostTestExplorer(controller, targets, runner);
  context.subscriptions.push(controller);

  const refreshViews = async (): Promise<void> => {
    tree.refresh();
    await explorer.discover();
  };

  context.subscriptions.push(
    vscode.window.registerTreeDataProvider('frostTargets', tree),
    vscode.commands.registerCommand('frost.refreshDaemonStatus', () =>
      daemonStatus.refresh(),
    ),
    vscode.tasks.registerTaskProvider(
      TASK_TYPE,
      new FrostTaskProvider(async (folder) => {
        const list = await targets.list(folder);
        return list.map((target) => ({
          label: target.label,
          isTest: isTestKind(target.kind),
        }));
      }),
    ),
    vscode.debug.registerDebugConfigurationProvider(
      DEBUG_TYPE,
      new FrostDebugConfigurationProvider(status, output, (folder) =>
        pickTarget(targets, folder, 'all'),
      ),
    ),
    ...registerCommands({ targets, runner, refreshViews }),
    vscode.workspace.onDidChangeConfiguration((event) => {
      // Scoped to this extension's section: clearing on every setting change
      // would re-enumerate targets for a colour theme.
      if (event.affectsConfiguration('frostbuild')) {
        targets.clear();
        refreshViews();
        void daemonStatus.refresh();
      }
    }),
    vscode.workspace.onDidSaveTextDocument((document) => {
      const folder = frostFolder(document.uri);
      if (!folder || !readSettings(folder.uri).buildOnSave) {
        return;
      }
      void vscode.commands.executeCommand('frost.buildFileOwners', document.uri);
    }),
  );

  status.idle();
  void daemonStatus.refresh();
}

export function deactivate(): void {
  // Nothing to unwind: every disposable is owned by the extension context.
}
