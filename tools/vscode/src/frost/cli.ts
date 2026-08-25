// The one place that spawns frost. Everything else in `src/frost/` is pure and
// takes already-captured text, which is what makes it testable without a VS
// Code instance or a built binary.

import { spawn } from 'node:child_process';

import type { FrostDaemonStatus, FrostInfo, QueryResult } from './types';

export interface FrostRun {
  code: number;
  stdout: string;
  stderr: string;
  /** stdout and stderr interleaved is not recoverable after the fact, so
   *  callers that parse progress read this concatenation instead. */
  output: string;
}

export interface FrostCliOptions {
  /** Path to the frost binary. `frost` resolves through PATH. */
  binary?: string;
  /** Workspace root passed as `-C`. */
  cwd: string;
  /** Extra environment on top of the current process's. */
  env?: NodeJS.ProcessEnv;
  signal?: AbortSignal;
}

/**
 * Run frost and capture its output.
 *
 * A non-zero exit is a normal result, not an exception: `build` failing and
 * `query` finding nothing both exit non-zero and both have output worth
 * reading. Only a frost that could not be started at all rejects.
 */
export function runFrost(
  args: string[],
  options: FrostCliOptions,
): Promise<FrostRun> {
  const binary = options.binary ?? 'frost';
  return new Promise((resolve, reject) => {
    const child = spawn(binary, ['-C', options.cwd, ...args], {
      env: { ...process.env, ...options.env },
      signal: options.signal,
      windowsHide: true,
    });
    let stdout = '';
    let stderr = '';
    let output = '';
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk: string) => {
      stdout += chunk;
      output += chunk;
    });
    child.stderr.on('data', (chunk: string) => {
      stderr += chunk;
      output += chunk;
    });
    child.on('error', reject);
    child.on('close', (code) => {
      resolve({ code: code ?? 1, stdout, stderr, output });
    });
  });
}

/** `frost info --json`. */
export async function readInfo(options: FrostCliOptions): Promise<FrostInfo> {
  const run = await runFrost(['info', '--json'], options);
  if (run.code !== 0) {
    throw new Error(`frost info failed (${run.code}): ${run.output.trim()}`);
  }
  return JSON.parse(run.stdout) as FrostInfo;
}

/** `frost daemon status --json`. A stopped daemon is a successful result. */
export async function readDaemonStatus(
  options: FrostCliOptions,
): Promise<FrostDaemonStatus> {
  const run = await runFrost(['daemon', 'status', '--json'], options);
  if (run.code !== 0) {
    throw new Error(
      `frost daemon status failed (${run.code}): ${run.output.trim()}`,
    );
  }
  return parseDaemonStatus(run.stdout);
}

/** Validate the small contract before editor code trusts an arbitrary binary. */
export function parseDaemonStatus(text: string): FrostDaemonStatus {
  const value: unknown = JSON.parse(text);
  if (!value || typeof value !== 'object') {
    throw new Error('frost daemon status returned a non-object');
  }
  const candidate = value as Partial<FrostDaemonStatus>;
  const states: FrostDaemonStatus['state'][] = [
    'running',
    'stopped',
    'protocol_mismatch',
  ];
  if (
    candidate.schema !== 'frost-daemon-status-v1' ||
    !candidate.state ||
    !states.includes(candidate.state) ||
    (candidate.protocol !== null && typeof candidate.protocol !== 'number') ||
    typeof candidate.expected_protocol !== 'number'
  ) {
    throw new Error('frost daemon status returned an unsupported payload');
  }
  return candidate as FrostDaemonStatus;
}

/**
 * `frost query <args...> --json`.
 *
 * An empty result exits 1 by design (the "nothing matched" convention shared
 * with `somepath`), so that is returned as an empty target list rather than
 * raised: a file with no owning target is an answer.
 */
export async function query(
  args: string[],
  options: FrostCliOptions,
): Promise<QueryResult> {
  const run = await runFrost(['query', ...args, '--json'], options);
  if (run.code === 0) {
    return JSON.parse(run.stdout) as QueryResult;
  }
  if (run.stdout.trim() === '') {
    return { query: args.join(' '), targets: [] };
  }
  throw new Error(`frost query failed (${run.code}): ${run.output.trim()}`);
}

/** `frost query <args...> --output label-kind`, returned as raw lines. */
export async function queryLabelKind(
  args: string[],
  options: FrostCliOptions,
): Promise<string> {
  const run = await runFrost(
    ['query', ...args, '--output', 'label-kind'],
    options,
  );
  if (run.code !== 0 && run.stdout.trim() === '') {
    return '';
  }
  return run.stdout;
}

/**
 * Every target in the workspace, or `undefined` if frost could not answer.
 *
 * The distinction matters and is why this does not reuse `queryLabelKind`: a
 * workspace with no targets and a frost that failed to run both produce no
 * output, and only the first is a result worth caching. Caching the second
 * makes correcting `frostbuild.binaryPath` appear to do nothing.
 */
export async function queryTargets(
  args: string[],
  options: FrostCliOptions,
): Promise<string | undefined> {
  const run = await runFrost(
    ['query', 'targets', ...args, '--output', 'label-kind'],
    options,
  );
  return run.code === 0 ? run.stdout : undefined;
}
