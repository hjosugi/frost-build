// Shapes the extension reads out of frost. Every one of these corresponds to a
// documented CLI surface in docs/28_compatibility_contract.md: the `--json`
// payloads and the `--output label-kind` form are promises, the human-facing
// progress text is not. Where a module has to read prose (the failure output of
// a compiler, for instance) that is called out where it happens.

/** `frost info --json`. Additive by contract, so unknown keys are kept. */
export interface FrostInfo {
  version: string;
  workspace_root: string;
  manifest: string;
  config: string;
  output_dir: string;
  bin_dir: string;
  obj_dir: string;
  tmp_dir: string;
  cas_dir: string;
  journal: string;
  graph_store: string;
  hash_cache: string;
  daemon_socket: string;
  action_key_schema: string;
  [key: string]: string;
}

/** `frost query <fn> --json`. `paths`/`truncated` appear for `allpaths` only. */
export interface QueryResult {
  query: string;
  targets: string[];
  paths?: string[][];
  truncated?: boolean;
}

/** Every target kind `frost.toml` accepts. */
export type TargetKind =
  | 'cc_binary'
  | 'cc_library'
  | 'cc_test'
  | 'genrule'
  | 'test'
  | 'kofun_binary'
  | 'command';

/** One line of `frost query <fn> --output label-kind`. */
export interface LabeledTarget {
  kind: TargetKind;
  /** Full label as frost prints it: `//pkg:name`, or a bare name at the root. */
  label: string;
  /** Package path, `''` for a root target. */
  packagePath: string;
  /** Target name without the package. */
  name: string;
}

/** Severity as an editor understands it. */
export type DiagnosticSeverity = 'error' | 'warning' | 'info';

/** One diagnostic located in a file, ready to become a VS Code Diagnostic. */
export interface FrostDiagnostic {
  /** Workspace-relative, `/`-separated. */
  file: string;
  /** 1-based, as compilers report. The VS Code layer converts to 0-based. */
  line: number;
  /** 1-based; absent when the compiler reported only a line. */
  column?: number;
  severity: DiagnosticSeverity;
  message: string;
  /** Target whose action produced it, when frost attributed the failure. */
  target?: string;
}

/** What a run of `frost build`/`frost test` reported, parsed from its output. */
export interface BuildOutcome {
  diagnostics: FrostDiagnostic[];
  /** Action ids frost reported as failed, in the order it reported them. */
  failedActions: string[];
  /** The `frost: ...` summary line, verbatim, when one was printed. */
  summary?: string;
  tests?: TestSummary;
}

/** The `tests: N passed, N failed, N cached` line. */
export interface TestSummary {
  passed: number;
  failed: number;
  cached: number;
}

/** A test target, with its shards when it declares more than one. */
export interface TestItem {
  label: string;
  kind: 'test' | 'cc_test';
  packagePath: string;
  name: string;
  /** One entry per shard; a single unsharded entry when `shard_count` is 1. */
  shards: TestShard[];
}

export interface TestShard {
  /** Action id frost uses: `test:NAME` or `test:NAME#i/n`. */
  actionId: string;
  /** 0-based; `undefined` when the target is not sharded. */
  index?: number;
  total: number;
}
