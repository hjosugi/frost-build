// Settings and workspace resolution. Kept separate from `extension.ts` so the
// VS Code layer stays a thin translation of the pure modules rather than a
// place where configuration reading is interleaved with command handling.

import * as vscode from 'vscode';

import type { FrostCliOptions } from './frost/cli';

export interface FrostSettings {
  binaryPath: string;
  profile: string;
  /** Empty means the host platform, which is what frost does without a flag. */
  platform: string;
  buildOnSave: boolean;
}

export function readSettings(scope?: vscode.Uri): FrostSettings {
  const config = vscode.workspace.getConfiguration('frostbuild', scope);
  return {
    binaryPath: config.get<string>('binaryPath', 'frost'),
    profile: config.get<string>('profile', 'debug'),
    platform: config.get<string>('platform', ''),
    buildOnSave: config.get<boolean>('buildOnSave', false),
  };
}

/**
 * The workspace folder a frost command should run in.
 *
 * A multi-root window can hold several frost workspaces, so the folder is
 * chosen from the active document when there is one. Falling back to the first
 * folder is only correct when there is exactly one candidate, which is why the
 * caller is expected to have a URI in hand for anything file-specific.
 */
export function frostFolder(
  resource?: vscode.Uri,
): vscode.WorkspaceFolder | undefined {
  const folders = vscode.workspace.workspaceFolders;
  if (!folders || folders.length === 0) {
    return undefined;
  }
  if (resource) {
    const owning = vscode.workspace.getWorkspaceFolder(resource);
    if (owning) {
      return owning;
    }
  }
  const active = vscode.window.activeTextEditor?.document.uri;
  if (active) {
    const owning = vscode.workspace.getWorkspaceFolder(active);
    if (owning) {
      return owning;
    }
  }
  return folders[0];
}

export function cliOptions(
  folder: vscode.WorkspaceFolder,
  settings: FrostSettings,
): FrostCliOptions {
  return { binary: settings.binaryPath, cwd: folder.uri.fsPath };
}

/** The `--profile`/`--platform` flags a command should carry, if any. */
export function configurationArgs(settings: FrostSettings): string[] {
  const args = ['--profile', settings.profile];
  if (settings.platform !== '') {
    args.push('--platform', settings.platform);
  }
  return args;
}

/**
 * Workspace-relative, `/`-separated path for a file inside `folder`.
 *
 * frost speaks workspace-relative paths everywhere — `query owners` takes them
 * and diagnostics report them — so this is the one conversion the VS Code layer
 * owes the pure modules.
 */
export function relativePath(
  folder: vscode.WorkspaceFolder,
  file: vscode.Uri,
): string | undefined {
  const root = folder.uri.fsPath.replace(/\\/g, '/').replace(/\/$/, '');
  const target = file.fsPath.replace(/\\/g, '/');
  if (!target.startsWith(root + '/')) {
    return undefined;
  }
  return target.slice(root.length + 1);
}
