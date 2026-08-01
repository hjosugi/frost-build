// The task provider. Tasks rather than a bespoke terminal because VS Code
// already knows how to show, re-run and cancel them, and because a task is what
// a keybinding or a launch configuration can depend on.

import * as vscode from 'vscode';

import { configurationArgs, readSettings, type FrostSettings } from './workspace';

export const TASK_TYPE = 'frost';

export interface FrostTaskDefinition extends vscode.TaskDefinition {
  type: typeof TASK_TYPE;
  command: 'build' | 'test' | 'run';
  target?: string;
  profile?: string;
  platform?: string;
}

/**
 * Build the argv for one task.
 *
 * `--no-tui` is not optional here: the dashboard redraws in place, which is
 * right for a terminal a human is watching and wrong for output another program
 * has to read line by line. The problem matcher depends on the stable form.
 */
export function taskArgs(
  definition: FrostTaskDefinition,
  settings: FrostSettings,
): string[] {
  const args: string[] = [definition.command];
  if (definition.target) {
    args.push(definition.target);
  }
  const profile = definition.profile ?? settings.profile;
  args.push('--profile', profile);
  const platform = definition.platform ?? settings.platform;
  if (platform !== '') {
    args.push('--platform', platform);
  }
  if (definition.command !== 'run') {
    args.push('--no-tui');
  }
  return args;
}

function taskName(definition: FrostTaskDefinition): string {
  return definition.target
    ? `${definition.command} ${definition.target}`
    : `${definition.command} (default targets)`;
}

export function createTask(
  definition: FrostTaskDefinition,
  folder: vscode.WorkspaceFolder,
  settings: FrostSettings,
): vscode.Task {
  const args = taskArgs(definition, settings);
  const execution = new vscode.ProcessExecution(settings.binaryPath, args, {
    cwd: folder.uri.fsPath,
  });
  const task = new vscode.Task(
    definition,
    folder,
    taskName(definition),
    'frost',
    execution,
    // The matcher lives in the extension rather than in package.json because
    // frost attributes a compiler's output to a target, and a declarative
    // matcher cannot see that framing. See diagnostics.ts.
    [],
  );
  task.group =
    definition.command === 'test'
      ? vscode.TaskGroup.Test
      : vscode.TaskGroup.Build;
  return task;
}

/**
 * Offers one build task per target plus a default build, and one test task per
 * test target. The list is supplied by the caller because enumerating targets
 * means running frost, and a task provider must return promptly.
 */
export class FrostTaskProvider implements vscode.TaskProvider {
  constructor(
    private readonly listTargets: (
      folder: vscode.WorkspaceFolder,
    ) => Promise<{ label: string; isTest: boolean }[]>,
  ) {}

  async provideTasks(): Promise<vscode.Task[]> {
    const folders = vscode.workspace.workspaceFolders ?? [];
    const tasks: vscode.Task[] = [];
    for (const folder of folders) {
      const settings = readSettings(folder.uri);
      tasks.push(
        createTask({ type: TASK_TYPE, command: 'build' }, folder, settings),
      );
      let targets: { label: string; isTest: boolean }[] = [];
      try {
        targets = await this.listTargets(folder);
      } catch {
        // A folder that is not a frost workspace, or a frost that failed to
        // run, contributes no tasks rather than failing the whole provider —
        // one broken folder in a multi-root window must not empty the list.
        continue;
      }
      for (const target of targets) {
        tasks.push(
          createTask(
            { type: TASK_TYPE, command: 'build', target: target.label },
            folder,
            settings,
          ),
        );
        if (target.isTest) {
          tasks.push(
            createTask(
              { type: TASK_TYPE, command: 'test', target: target.label },
              folder,
              settings,
            ),
          );
        }
      }
    }
    return tasks;
  }

  /** Completes a task the user wrote in tasks.json by hand. */
  resolveTask(task: vscode.Task): vscode.Task | undefined {
    const definition = task.definition as FrostTaskDefinition;
    if (definition.type !== TASK_TYPE || !definition.command) {
      return undefined;
    }
    const folder =
      task.scope !== vscode.TaskScope.Global &&
      task.scope !== vscode.TaskScope.Workspace
        ? (task.scope as vscode.WorkspaceFolder | undefined)
        : undefined;
    if (!folder) {
      return undefined;
    }
    return createTask(definition, folder, readSettings(folder.uri));
  }
}

/** Exported for the CLI-shape test; keeps `configurationArgs` in one place. */
export { configurationArgs };
