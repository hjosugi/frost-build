// The target tree.
//
// The thing every one of the reference extensions has and this did not: a place
// to see what the workspace contains without knowing a label to type. The
// grouping and sorting are already computed and tested in `frost/targets.ts`;
// this is the binding.

import * as vscode from 'vscode';

import { buildTargetTree, isRunnableKind, isTestKind } from './frost/targets';
import type { TargetTreeNode } from './frost/targets';
import type { LabeledTarget } from './frost/types';
import type { TargetIndex } from './targets-provider';

type Node =
  | { kind: 'package'; folder: vscode.WorkspaceFolder; node: TargetTreeNode }
  | { kind: 'target'; folder: vscode.WorkspaceFolder; target: LabeledTarget }
  | { kind: 'folder'; folder: vscode.WorkspaceFolder };

export class TargetTreeProvider implements vscode.TreeDataProvider<Node> {
  private readonly changed = new vscode.EventEmitter<Node | undefined>();
  readonly onDidChangeTreeData = this.changed.event;

  constructor(private readonly targets: TargetIndex) {}

  refresh(): void {
    this.changed.fire(undefined);
  }

  getTreeItem(element: Node): vscode.TreeItem {
    if (element.kind === 'folder') {
      const item = new vscode.TreeItem(
        element.folder.name,
        vscode.TreeItemCollapsibleState.Expanded,
      );
      item.contextValue = 'frostWorkspace';
      return item;
    }
    if (element.kind === 'package') {
      const item = new vscode.TreeItem(
        element.node.displayName,
        vscode.TreeItemCollapsibleState.Expanded,
      );
      item.contextValue = 'frostPackage';
      item.resourceUri = element.node.packagePath
        ? vscode.Uri.joinPath(element.folder.uri, element.node.packagePath)
        : element.folder.uri;
      return item;
    }
    const target = element.target;
    const item = new vscode.TreeItem(
      target.name,
      vscode.TreeItemCollapsibleState.None,
    );
    item.description = target.kind;
    item.tooltip = target.label;
    // The context value is what lets package.json put "Build"/"Run"/"Debug" on
    // the right rows only, rather than offering to run a library.
    item.contextValue = isTestKind(target.kind)
      ? 'frostTestTarget'
      : isRunnableKind(target.kind)
        ? 'frostRunnableTarget'
        : 'frostTarget';
    item.iconPath = new vscode.ThemeIcon(iconFor(target));
    // Clicking a row builds it: the single most common thing to want, and one
    // click rather than a context menu.
    item.command = {
      command: 'frost.buildTarget',
      title: 'Build',
      arguments: [target.label, element.folder],
    };
    return item;
  }

  async getChildren(element?: Node): Promise<Node[]> {
    const folders = vscode.workspace.workspaceFolders ?? [];
    if (!element) {
      // A single-folder window shows packages directly; a multi-root one needs
      // the extra level to say which workspace a target belongs to.
      if (folders.length === 1) {
        return this.packageChildren(folders[0]);
      }
      return folders.map((folder) => ({ kind: 'folder', folder }));
    }
    if (element.kind === 'folder') {
      return this.packageChildren(element.folder);
    }
    if (element.kind === 'package') {
      return [
        ...element.node.children.map((child) => ({
          kind: 'package' as const,
          folder: element.folder,
          node: child,
        })),
        ...element.node.targets.map((target) => ({
          kind: 'target' as const,
          folder: element.folder,
          target,
        })),
      ];
    }
    return [];
  }

  private async packageChildren(folder: vscode.WorkspaceFolder): Promise<Node[]> {
    const targets = await this.targets.list(folder);
    if (targets.length === 0) {
      return [];
    }
    const root = buildTargetTree(targets);
    return [
      ...root.children.map((child) => ({
        kind: 'package' as const,
        folder,
        node: child,
      })),
      ...root.targets.map((target) => ({
        kind: 'target' as const,
        folder,
        target,
      })),
    ];
  }
}

function iconFor(target: LabeledTarget): string {
  if (isTestKind(target.kind)) {
    return 'beaker';
  }
  if (isRunnableKind(target.kind)) {
    return 'play';
  }
  return target.kind === 'genrule' ? 'gear' : 'library';
}
