// A `vscode` module standing in for the real one, so the editor layer can be
// executed rather than only type-checked.
//
// The extension's logic lives in pure modules precisely so most of it needs no
// editor. What was left over — command registration, quick picks, turning a
// parsed diagnostic into a `Diagnostic` at the right URI — is small, but "small"
// is not "correct", and until this harness existed that half of the extension
// had never run at all.
//
// This is not a substitute for opening the extension in a real editor before
// publishing. It is the part that can run on every push.

import { posix } from 'node:path';

// `import * as Module` yields a namespace object whose properties are
// getter-only, so `_load` cannot be replaced through it. The CommonJS export is
// the real, mutable module object.
// eslint-disable-next-line @typescript-eslint/no-var-requires
const NodeModule = require('node:module') as {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  _load: (request: string, ...rest: any[]) => unknown;
};

/** Everything the stub saw, for tests to assert against. */
export interface Recorded {
  commands: Map<string, (...args: unknown[]) => unknown>;
  taskProviders: Map<string, unknown>;
  diagnostics: Map<string, StubDiagnostic[]>;
  diagnosticClears: number;
  status: { text: string; tooltip: string; shown: boolean };
  output: string[];
  info: string[];
  warnings: string[];
  errors: string[];
  saveHandlers: ((document: { uri: StubUri }) => unknown)[];
  configurationHandlers: ((event: {
    affectsConfiguration: (section: string) => boolean;
  }) => unknown)[];
  /** Answer handed back by `showQuickPick`; set per test. */
  quickPickAnswer: unknown;
  quickPickItems: unknown[][];
  configuration: Record<string, unknown>;
  workspaceFolders: { uri: StubUri; name: string; index: number }[];
}

export interface StubUri {
  scheme: string;
  fsPath: string;
  path: string;
  toString(): string;
}

export interface StubDiagnostic {
  message: string;
  severity: number;
  source?: string;
  range: { start: { line: number; character: number } };
}

function uri(fsPath: string): StubUri {
  const normalized = fsPath.replace(/\\/g, '/');
  return {
    scheme: 'file',
    fsPath: normalized,
    path: normalized,
    toString: () => `file://${normalized}`,
  };
}

/**
 * Install the stub and return the recorder.
 *
 * `Module._load` is patched rather than the require cache, because `vscode`
 * cannot be resolved at all outside a real editor — there is no file to cache
 * against. Restoring is the caller's job via the returned `dispose`.
 */
export function installVscodeStub(): { recorded: Recorded; dispose: () => void } {
  const recorded: Recorded = {
    commands: new Map(),
    taskProviders: new Map(),
    diagnostics: new Map(),
    diagnosticClears: 0,
    status: { text: '', tooltip: '', shown: false },
    output: [],
    info: [],
    warnings: [],
    errors: [],
    saveHandlers: [],
    configurationHandlers: [],
    quickPickAnswer: undefined,
    quickPickItems: [],
    configuration: {},
    workspaceFolders: [],
  };

  const disposable = { dispose: () => undefined };

  const stub = {
    Uri: {
      file: (path: string) => uri(path),
      parse: (value: string) => uri(value.replace(/^file:\/\//, '')),
      joinPath: (base: StubUri, ...parts: string[]) =>
        uri(posix.join(base.fsPath, ...parts)),
    },
    Range: class {
      constructor(
        public startLine: number,
        public startCharacter: number,
        public endLine: number,
        public endCharacter: number,
      ) {}
      get start() {
        return { line: this.startLine, character: this.startCharacter };
      }
    },
    Diagnostic: class {
      source?: string;
      constructor(
        public range: { start: { line: number; character: number } },
        public message: string,
        public severity: number,
      ) {}
    },
    DiagnosticSeverity: { Error: 0, Warning: 1, Information: 2, Hint: 3 },
    StatusBarAlignment: { Left: 1, Right: 2 },
    TaskGroup: { Build: 'build', Test: 'test' },
    TaskScope: { Global: 1, Workspace: 2 },
    ProcessExecution: class {
      constructor(
        public process: string,
        public args: string[],
        public options?: unknown,
      ) {}
    },
    Task: class {
      group: unknown;
      constructor(
        public definition: unknown,
        public scope: unknown,
        public name: string,
        public source: string,
        public execution: unknown,
        public problemMatchers: string[],
      ) {}
    },
    window: {
      createOutputChannel: () => ({
        appendLine: (line: string) => recorded.output.push(line),
        append: (text: string) => recorded.output.push(text),
        show: () => undefined,
        dispose: () => undefined,
      }),
      createStatusBarItem: () => ({
        get text() {
          return recorded.status.text;
        },
        set text(value: string) {
          recorded.status.text = value;
        },
        get tooltip() {
          return recorded.status.tooltip;
        },
        set tooltip(value: string) {
          recorded.status.tooltip = value;
        },
        command: '',
        show: () => {
          recorded.status.shown = true;
        },
        dispose: () => undefined,
      }),
      showQuickPick: (items: unknown[]) => {
        recorded.quickPickItems.push(items);
        return Promise.resolve(recorded.quickPickAnswer);
      },
      showInformationMessage: (message: string) => {
        recorded.info.push(message);
        return Promise.resolve(undefined);
      },
      showWarningMessage: (message: string) => {
        recorded.warnings.push(message);
        return Promise.resolve(undefined);
      },
      showErrorMessage: (message: string) => {
        recorded.errors.push(message);
        return Promise.resolve(undefined);
      },
      activeTextEditor: undefined as unknown,
    },
    languages: {
      createDiagnosticCollection: () => ({
        set: (target: StubUri, list: StubDiagnostic[]) => {
          recorded.diagnostics.set(target.toString(), list);
        },
        clear: () => {
          recorded.diagnosticClears += 1;
          recorded.diagnostics.clear();
        },
        dispose: () => undefined,
      }),
    },
    commands: {
      registerCommand: (id: string, handler: (...args: unknown[]) => unknown) => {
        recorded.commands.set(id, handler);
        return disposable;
      },
    },
    tasks: {
      registerTaskProvider: (type: string, provider: unknown) => {
        recorded.taskProviders.set(type, provider);
        return disposable;
      },
    },
    workspace: {
      get workspaceFolders() {
        return recorded.workspaceFolders.length > 0
          ? recorded.workspaceFolders
          : undefined;
      },
      getWorkspaceFolder: (target: StubUri) =>
        recorded.workspaceFolders.find((folder) =>
          target.fsPath.startsWith(folder.uri.fsPath),
        ),
      getConfiguration: () => ({
        get: (key: string, fallback: unknown) =>
          key in recorded.configuration ? recorded.configuration[key] : fallback,
      }),
      onDidSaveTextDocument: (handler: (document: { uri: StubUri }) => unknown) => {
        recorded.saveHandlers.push(handler);
        return disposable;
      },
      onDidChangeConfiguration: (
        handler: (event: { affectsConfiguration: (section: string) => boolean }) => unknown,
      ) => {
        recorded.configurationHandlers.push(handler);
        return disposable;
      },
    },
  };

  const original = NodeModule._load;
  NodeModule._load = function (request: string, ...rest: unknown[]) {
    if (request === 'vscode') {
      return stub;
    }
    return original.call(this, request, ...rest);
  };

  return {
    recorded,
    dispose: () => {
      NodeModule._load = original;
    },
  };
}

export function stubUri(fsPath: string): StubUri {
  return uri(fsPath);
}
