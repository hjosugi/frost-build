// The status item, behind a narrow interface.
//
// Split out so that the modules that report progress do not each need a handle
// on a VS Code object, and so a test can assert on what was reported without
// reaching into the editor API.

import * as vscode from 'vscode';

export interface StatusReporter {
  idle(): void;
  running(what: string): void;
  succeeded(detail: string): void;
  failed(detail: string): void;
}

export function createStatusReporter(): StatusReporter & vscode.Disposable {
  const item = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 0);
  item.command = 'frost.build';
  const set = (text: string, tooltip: string): void => {
    item.text = text;
    item.tooltip = tooltip;
    item.show();
  };
  return {
    idle: () => set('$(tools) frost', 'FrostBuild: ready'),
    running: (what) => set('$(sync~spin) frost', what),
    succeeded: (detail) => set('$(check) frost', detail),
    failed: (detail) => set('$(error) frost', detail),
    dispose: () => item.dispose(),
  };
}
