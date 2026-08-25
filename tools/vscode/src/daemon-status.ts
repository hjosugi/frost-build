// A deliberately small poller around the daemon's machine-readable status.
// The status bar retains the last build result independently, so polling can
// never erase a useful failure while keeping terminal-driven daemon changes
// visible without asking the user to reload the extension.

import * as vscode from 'vscode';

import { readDaemonStatus } from './frost/cli';
import type { StatusReporter } from './status';
import { cliOptions, frostFolder, readSettings } from './workspace';

export class DaemonStatusMonitor implements vscode.Disposable {
  private readonly timer: ReturnType<typeof setInterval>;
  private generation = 0;

  constructor(
    private readonly status: StatusReporter,
    intervalMs = 5_000,
  ) {
    this.timer = setInterval(() => void this.refresh(), intervalMs);
    // Node tests use the same class without a VS Code extension host owning the
    // process. The poller must not keep that process alive after assertions.
    this.timer.unref?.();
  }

  async refresh(): Promise<void> {
    const generation = ++this.generation;
    const folder = frostFolder();
    if (!folder) {
      this.status.daemon(undefined);
      return;
    }
    try {
      const settings = readSettings(folder.uri);
      const daemon = await readDaemonStatus(cliOptions(folder, settings));
      if (generation === this.generation) {
        this.status.daemon(daemon);
      }
    } catch {
      // An old/missing binary is already reported when a user runs a command.
      // Passive polling stays quiet and marks only this half of the indicator.
      if (generation === this.generation) {
        this.status.daemon(undefined);
      }
    }
  }

  dispose(): void {
    this.generation += 1;
    clearInterval(this.timer);
  }
}
