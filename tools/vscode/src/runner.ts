// Running frost and turning what it said into editor objects.
//
// One place, because every entry point — a command, the tree view, a test run,
// a save — must report the same way. Diagnostics that appear for one path and
// not another are how a Problems panel stops being believed.

import * as vscode from 'vscode';

import { runFrost } from './frost/cli';
import { normalizeDiagnosticPath, parseBuildOutput } from './frost/diagnostics';
import { isAbsolutePath } from './frost/paths';
import type { BuildOutcome, FrostDiagnostic } from './frost/types';
import type { StatusReporter } from './status';
import { cliOptions, configurationArgs, readSettings } from './workspace';

export interface RunOutcome {
  code: number;
  output: string;
  parsed: BuildOutcome;
}

export class FrostRunner {
  constructor(
    private readonly output: vscode.OutputChannel,
    private readonly diagnostics: vscode.DiagnosticCollection,
    private readonly status: StatusReporter,
  ) {}

  /**
   * Run a frost subcommand against a folder, echo it, and publish what it said.
   *
   * `--no-tui` is not optional: the dashboard redraws in place, which is right
   * for a terminal a human watches and wrong for output another program parses.
   */
  async run(
    folder: vscode.WorkspaceFolder,
    command: 'build' | 'test',
    targets: string[],
    extra: string[] = [],
  ): Promise<RunOutcome | undefined> {
    const settings = readSettings(folder.uri);
    const args = [
      command,
      ...targets,
      ...configurationArgs(settings),
      ...extra,
      '--no-tui',
    ];
    this.status.running(`frost ${command} ${targets.join(' ')}`);
    this.output.appendLine(`$ frost ${args.join(' ')}`);
    let run;
    try {
      run = await runFrost(args, cliOptions(folder, settings));
    } catch (error) {
      this.status.failed('FrostBuild: could not start frost');
      void vscode.window.showErrorMessage(
        `FrostBuild: could not run ${settings.binaryPath}: ${String(error)}`,
      );
      return undefined;
    }
    this.output.append(run.output);
    const parsed = parseBuildOutput(run.output);
    this.publish(folder, parsed.diagnostics);
    if (run.code === 0) {
      this.status.succeeded(parsed.summary ?? 'FrostBuild: up to date');
    } else {
      this.status.failed(parsed.summary ?? `FrostBuild: ${command} failed`);
      this.output.show(true);
    }
    return { code: run.code, output: run.output, parsed };
  }

  /**
   * Replace the whole collection each run rather than merging.
   *
   * A diagnostic frost no longer reports is fixed, and leaving it on screen
   * because this run happened not to mention that file is how a Problems panel
   * stops being trusted.
   */
  publish(folder: vscode.WorkspaceFolder, items: FrostDiagnostic[]): void {
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
      const diagnostic = new vscode.Diagnostic(
        new vscode.Range(line, column, line, Number.MAX_SAFE_INTEGER),
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
    this.diagnostics.clear();
    for (const [uri, list] of byFile) {
      this.diagnostics.set(vscode.Uri.parse(uri), list);
    }
  }
}

function severityOf(
  severity: FrostDiagnostic['severity'],
): vscode.DiagnosticSeverity {
  switch (severity) {
    case 'error':
      return vscode.DiagnosticSeverity.Error;
    case 'warning':
      return vscode.DiagnosticSeverity.Warning;
    default:
      return vscode.DiagnosticSeverity.Information;
  }
}
