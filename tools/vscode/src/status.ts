// The status item, behind a narrow interface.
//
// Split out so that the modules that report progress do not each need a handle
// on a VS Code object, and so a test can assert on what was reported without
// reaching into the editor API.

import * as vscode from 'vscode';

import type { FrostDaemonStatus } from './frost/types';

export type DaemonIndicator = FrostDaemonStatus | undefined;

export interface StatusReporter {
  idle(): void;
  running(what: string): void;
  succeeded(detail: string): void;
  failed(detail: string): void;
  daemon(status: DaemonIndicator): void;
}

export function createStatusReporter(): StatusReporter & vscode.Disposable {
  const item = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 0);
  item.command = 'frost.refreshDaemonStatus';
  let activity = { icon: '$(tools)', detail: 'FrostBuild: ready' };
  let daemon: DaemonIndicator;
  const render = (): void => {
    let daemonText = '$(question) frostd';
    let daemonTooltip = 'Daemon: status unavailable';
    if (daemon?.state === 'running') {
      daemonText = '$(vm-active) frostd';
      daemonTooltip = `Daemon: running (protocol ${daemon.protocol})`;
    } else if (daemon?.state === 'stopped') {
      daemonText = '$(circle-slash) frostd';
      daemonTooltip = 'Daemon: stopped';
    } else if (daemon?.state === 'protocol_mismatch') {
      daemonText = '$(warning) frostd';
      daemonTooltip = `Daemon: protocol ${daemon.protocol}; frost expects ${daemon.expected_protocol}`;
    }
    item.text = `${activity.icon} frost · ${daemonText}`;
    item.tooltip = `${activity.detail}\n${daemonTooltip}`;
    item.show();
  };
  const setActivity = (icon: string, detail: string): void => {
    activity = { icon, detail };
    render();
  };
  return {
    idle: () => setActivity('$(tools)', 'FrostBuild: ready'),
    running: (what) => setActivity('$(sync~spin)', what),
    succeeded: (detail) => setActivity('$(check)', detail),
    failed: (detail) => setActivity('$(error)', detail),
    daemon: (status) => {
      daemon = status;
      render();
    },
    dispose: () => item.dispose(),
  };
}
