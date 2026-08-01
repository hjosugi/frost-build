// Debugging, by delegation.
//
// This extension does not implement a debug adapter and should not. C/C++ has
// one in ms-vscode.cpptools, Java in vscode-java-debug, Node's is built in,
// Python's is ms-python.debugpy — all of them better than anything this project
// would maintain. What was missing is the step before: knowing which adapter a
// target needs, where its artifact ends up, and building it first.
//
// frost already answers all three. `frost ide <target> --dry-run` builds the
// target and prints the launch configuration it computed, including the
// debugger flavour chosen from the artifact's extension (jar → java, .js →
// node, .py → debugpy, otherwise cppdbg with gdb or lldb by host). So the
// provider runs that and hands the result to VS Code, which starts the real
// debugger. Reimplementing the artifact-path and flavour rules in TypeScript is
// exactly what would make this a heavy extension, and it would be a second
// source of truth that drifts.
//
// This is the same shape vscode-maven and vscode-gradle use for Java: resolve a
// configuration, delegate the session.

import * as vscode from 'vscode';

import { runFrost } from './frost/cli';
import {
  firstLaunchConfiguration,
  frostErrorMessage,
  parseIdeOutput,
  requiredExtension,
} from './frost/launch';
import type { StatusReporter } from './status';
import { cliOptions, configurationArgs, readSettings } from './workspace';

export const DEBUG_TYPE = 'frost';

interface FrostDebugConfiguration extends vscode.DebugConfiguration {
  target?: string;
}

export class FrostDebugConfigurationProvider
  implements vscode.DebugConfigurationProvider
{
  constructor(
    private readonly status: StatusReporter,
    private readonly output: vscode.OutputChannel,
    private readonly pickTarget: (
      folder: vscode.WorkspaceFolder,
    ) => Promise<string | undefined>,
  ) {}

  /**
   * Offer a starting point when the user has no launch.json.
   *
   * Deliberately one entry with no target: resolving the target list here would
   * run frost during the debug dropdown's construction, which must be instant.
   * `resolveDebugConfiguration` asks for the target instead, when the user has
   * actually chosen to debug something.
   */
  provideDebugConfigurations(): vscode.DebugConfiguration[] {
    return [
      {
        type: DEBUG_TYPE,
        request: 'launch',
        name: 'Frost: debug target',
      },
    ];
  }

  /**
   * Turn a `frost` configuration into the real one.
   *
   * Returning `undefined` aborts the session silently, which is right after we
   * have already shown a message; returning `null` would open launch.json,
   * which is not where any of these problems are fixed.
   */
  async resolveDebugConfiguration(
    folder: vscode.WorkspaceFolder | undefined,
    configuration: FrostDebugConfiguration,
  ): Promise<vscode.DebugConfiguration | undefined> {
    if (configuration.type !== DEBUG_TYPE) {
      return configuration;
    }
    if (!folder) {
      void vscode.window.showErrorMessage(
        'FrostBuild: debugging needs an open workspace folder.',
      );
      return undefined;
    }
    const target = configuration.target ?? (await this.pickTarget(folder));
    if (!target) {
      return undefined;
    }

    const settings = readSettings(folder.uri);
    // `ide` builds the target before printing, which is what makes the artifact
    // exist and its extension — and therefore the debugger flavour — knowable.
    const args = ['ide', target, ...configurationArgs(settings), '--dry-run'];
    this.status.running(`frost ide ${target}`);
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

    if (run.code !== 0) {
      this.status.failed(`FrostBuild: cannot debug ${target}`);
      // frost's own message is the useful one — "not compiled with debug
      // symbols in profile debug; add [profile.debug] cflags = [...]" tells the
      // user exactly what to change. Replacing it with a generic failure would
      // throw away the only actionable part.
      const detail = frostErrorMessage(run.output) ?? 'see the FrostBuild output';
      void vscode.window.showErrorMessage(`FrostBuild: ${detail}`);
      this.output.show(true);
      return undefined;
    }

    const files = parseIdeOutput(run.output);
    const resolved = files ? firstLaunchConfiguration(files) : undefined;
    if (!resolved) {
      this.status.failed(`FrostBuild: cannot debug ${target}`);
      void vscode.window.showErrorMessage(
        `FrostBuild: frost produced no launch configuration for ${target}.`,
      );
      this.output.show(true);
      return undefined;
    }

    const needed = requiredExtension(resolved.type);
    if (needed && !vscode.extensions.getExtension(needed.id)) {
      // VS Code's own message for a missing adapter names the type, not the
      // extension, which leaves the user to work out that `cppdbg` means
      // installing C/C++. Say it.
      void vscode.window.showErrorMessage(
        `FrostBuild: debugging ${target} needs the ${needed.name} extension (${needed.id}).`,
      );
      return undefined;
    }

    this.status.succeeded(`FrostBuild: debugging ${target}`);
    // The build already happened above, so the preLaunchTask frost suggests
    // would run it a second time before the session starts.
    const { preLaunchTask: _preLaunchTask, ...launch } = resolved;
    return launch as vscode.DebugConfiguration;
  }
}
