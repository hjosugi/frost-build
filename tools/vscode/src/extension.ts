// The VS Code layer. Everything here is translation: it turns editor events
// into frost invocations and frost output into editor objects. All of the
// parsing lives in `src/frost/`, which never imports this module's dependency,
// so the logic can be tested without an editor.

import * as vscode from 'vscode';

import { queryLabelKind, runFrost } from './frost/cli';
import { parseBuildOutput, normalizeDiagnosticPath } from './frost/diagnostics';
import { parseDotGraph, rootTargets } from './frost/graph';
import { isAbsolutePath } from './frost/paths';
import {
  buildTargetTree,
  isTestKind,
  parseLabelKind,
  type TargetTreeNode,
} from './frost/targets';
import type { FrostDiagnostic, LabeledTarget } from './frost/types';
import { FrostTaskProvider, TASK_TYPE, taskArgs } from './tasks';
import {
  cliOptions,
  configurationArgs,
  frostFolder,
  readSettings,
  relativePath,
} from './workspace';

let output: vscode.OutputChannel;
let diagnostics: vscode.DiagnosticCollection;
let status: vscode.StatusBarItem;

/** Targets are cached per folder: enumerating them runs frost, and a task
 *  provider or a quick pick must not pay that on every keystroke. */
const targetCache = new Map<string, LabeledTarget[]>();

export function activate(context: vscode.ExtensionContext): void {
  output = vscode.window.createOutputChannel('FrostBuild');
  diagnostics = vscode.languages.createDiagnosticCollection('frost');
  status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 0);
  status.command = 'frost.build';
  context.subscriptions.push(output, diagnostics, status);

  context.subscriptions.push(
    vscode.tasks.registerTaskProvider(
      TASK_TYPE,
      new FrostTaskProvider(async (folder) => {
        const targets = await listTargets(folder);
        return targets.map((target) => ({
          label: target.label,
          isTest: isTestKind(target.kind),
        }));
      }),
    ),
    vscode.commands.registerCommand('frost.build', () => pickAndRun('build')),
    vscode.commands.registerCommand('frost.test', () => pickAndRun('test')),
    vscode.commands.registerCommand('frost.buildFileOwners', (uri?: vscode.Uri) =>
      buildFileOwners(uri ?? vscode.window.activeTextEditor?.document.uri),
    ),
    vscode.commands.registerCommand('frost.refreshTargets', () => {
      targetCache.clear();
      void vscode.window.showInformationMessage('FrostBuild: target list refreshed.');
    }),
    vscode.workspace.onDidSaveTextDocument((document) => {
      const folder = frostFolder(document.uri);
      if (!folder || !readSettings(folder.uri).buildOnSave) {
        return;
      }
      void buildFileOwners(document.uri);
    }),
  );

  setStatus('$(tools) frost', 'FrostBuild: ready');
}

export function deactivate(): void {
  targetCache.clear();
}

function setStatus(text: string, tooltip: string): void {
  status.text = text;
  status.tooltip = tooltip;
  status.show();
}

/**
 * Every target in a folder, with its kind.
 *
 * Two calls rather than one because frost has no primitive for "list the
 * universe": `query deps`/`rdeps` both need a starting target and `pick` needs
 * fzf. So `graph --dot` supplies the topology, its roots are derived, and
 * `query deps <root> --output label-kind` supplies the kinds — `label-kind`
 * rather than `--json` because the JSON form carries only names, and a tree
 * that cannot tell a test from a library is not worth showing.
 *
 * Roots are usually one or two, so this is a couple of invocations, cached.
 * A `frost query targets` that answered it in one call would replace all of
 * this; see the note in graph.ts.
 */
async function listTargets(
  folder: vscode.WorkspaceFolder,
): Promise<LabeledTarget[]> {
  const key = folder.uri.toString();
  const cached = targetCache.get(key);
  if (cached) {
    return cached;
  }
  const settings = readSettings(folder.uri);
  const options = cliOptions(folder, settings);
  const dot = await runFrost(
    ['graph', '--dot', ...configurationArgs(settings)],
    options,
  ).catch(() => undefined);
  // A folder that is not a frost workspace produces nothing here, which is the
  // answer rather than an error: a multi-root window may hold both.
  if (!dot || dot.code !== 0) {
    targetCache.set(key, []);
    return [];
  }
  const roots = rootTargets(parseDotGraph(dot.stdout));
  const byLabel = new Map<string, LabeledTarget>();
  for (const root of roots) {
    const text = await queryLabelKind(['deps', root], options).catch(() => '');
    for (const target of parseLabelKind(text)) {
      byLabel.set(target.label, target);
    }
  }
  const targets = [...byLabel.values()].sort((a, b) =>
    a.label < b.label ? -1 : a.label > b.label ? 1 : 0,
  );
  targetCache.set(key, targets);
  return targets;
}

async function pickAndRun(command: 'build' | 'test'): Promise<void> {
  const folder = frostFolder();
  if (!folder) {
    void vscode.window.showWarningMessage('FrostBuild: no workspace folder is open.');
    return;
  }
  const targets = await listTargets(folder);
  const choices = targets.filter(
    (target) => command === 'build' || isTestKind(target.kind),
  );
  if (choices.length === 0) {
    void vscode.window.showWarningMessage(
      `FrostBuild: no ${command === 'test' ? 'test ' : ''}targets found in ${folder.name}.`,
    );
    return;
  }
  const picked = await vscode.window.showQuickPick(
    choices.map((target) => ({
      label: target.label,
      description: target.kind,
    })),
    { placeHolder: `frost ${command}` },
  );
  if (!picked) {
    return;
  }
  await runAndReport(folder, command, [picked.label]);
}

async function buildFileOwners(uri: vscode.Uri | undefined): Promise<void> {
  if (!uri) {
    return;
  }
  const folder = frostFolder(uri);
  if (!folder) {
    return;
  }
  const relative = relativePath(folder, uri);
  if (!relative) {
    return;
  }
  const settings = readSettings(folder.uri);
  const text = await queryLabelKind(
    ['owners', relative],
    cliOptions(folder, settings),
  ).catch(() => '');
  const owners = parseLabelKind(text);
  if (owners.length === 0) {
    // Not an error: a header frost only learns about from a depfile has no
    // owning target in the configuration. Say which, so the empty answer does
    // not look like a broken command.
    void vscode.window.showInformationMessage(
      `FrostBuild: no target declares ${relative} among its inputs.`,
    );
    return;
  }
  await runAndReport(
    folder,
    'build',
    owners.map((owner) => owner.label),
  );
}

/** Run frost, echo its output, and turn what it reported into diagnostics. */
async function runAndReport(
  folder: vscode.WorkspaceFolder,
  command: 'build' | 'test',
  targets: string[],
): Promise<void> {
  const settings = readSettings(folder.uri);
  const args = [command, ...targets, ...configurationArgs(settings), '--no-tui'];
  setStatus('$(sync~spin) frost', `frost ${command} ${targets.join(' ')}`);
  output.appendLine(`$ frost ${args.join(' ')}`);
  let run;
  try {
    run = await runFrost(args, cliOptions(folder, settings));
  } catch (error) {
    setStatus('$(error) frost', 'FrostBuild: could not start frost');
    void vscode.window.showErrorMessage(
      `FrostBuild: could not run ${settings.binaryPath}: ${String(error)}`,
    );
    return;
  }
  output.append(run.output);
  const outcome = parseBuildOutput(run.output);
  publishDiagnostics(folder, outcome.diagnostics);
  if (run.code === 0) {
    setStatus('$(check) frost', outcome.summary ?? 'FrostBuild: up to date');
  } else {
    setStatus('$(error) frost', outcome.summary ?? 'FrostBuild: build failed');
    output.show(true);
  }
}

/**
 * Replace the whole collection each run rather than merging.
 *
 * A diagnostic frost no longer reports is fixed, and leaving it on screen
 * because this run happened not to mention that file is how a Problems panel
 * stops being trusted.
 */
function publishDiagnostics(
  folder: vscode.WorkspaceFolder,
  items: FrostDiagnostic[],
): void {
  const byFile = new Map<string, vscode.Diagnostic[]>();
  for (const item of items) {
    const path = normalizeDiagnosticPath(item.file);
    // A compiler reporting a system header gives an absolute path. Joining it
    // onto the workspace root would produce a file that does not exist, and
    // the diagnostic would attach to nothing at all.
    const uri = (
      isAbsolutePath(path)
        ? vscode.Uri.file(path)
        : vscode.Uri.joinPath(folder.uri, path)
    ).toString();
    const line = Math.max(0, item.line - 1);
    const column = Math.max(0, (item.column ?? 1) - 1);
    const range = new vscode.Range(line, column, line, Number.MAX_SAFE_INTEGER);
    const diagnostic = new vscode.Diagnostic(
      range,
      item.message,
      severityOf(item.severity),
    );
    diagnostic.source = item.target ? `frost (${item.target})` : 'frost';
    const list = byFile.get(uri);
    if (list) {
      list.push(diagnostic);
    } else {
      byFile.set(uri, [diagnostic]);
    }
  }
  diagnostics.clear();
  for (const [uri, list] of byFile) {
    diagnostics.set(vscode.Uri.parse(uri), list);
  }
}

function severityOf(severity: FrostDiagnostic['severity']): vscode.DiagnosticSeverity {
  switch (severity) {
    case 'error':
      return vscode.DiagnosticSeverity.Error;
    case 'warning':
      return vscode.DiagnosticSeverity.Warning;
    default:
      return vscode.DiagnosticSeverity.Information;
  }
}

/** Re-exported so the tree view can be added without touching this file. */
export type { TargetTreeNode };
export { buildTargetTree, taskArgs };
