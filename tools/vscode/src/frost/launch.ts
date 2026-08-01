// Reading the debug configuration frost already computed.
//
// The extension does not implement a debug adapter and must not grow one.
// `frost ide <target> --dry-run` prints the `launch.json` and `tasks.json` it
// would write, so the editor's job is to hand that configuration to whichever
// debugger extension owns the language — ms-vscode.cpptools for `cppdbg`,
// vscode-java-debug for `java`, VS Code itself for `node`, ms-python for
// `debugpy`. That delegation is the difference between a light extension and a
// heavy one, and it is also the only way the launch stays correct: frost knows
// the artifact path, the profile's output directory and the target's flavor,
// and every one of those would have to be reimplemented (and kept in step) to
// build the configuration here instead.
//
// So this module is a parser and nothing more. Pure, like the rest of
// `src/frost/`: it is handed text `cli.ts` captured and returns plain data.
//
// One thing about that text drives most of the code below. `frost ide` builds
// the target before it prints, so stdout starts with a build summary line —
// `frost: up to date · 9 of 12 actions · 1 ms` — and only then the JSON. A
// plain `JSON.parse` of the captured output throws on the first character.
// Callers may also pass the stdout/stderr concatenation (`FrostRun.output`),
// which is the only text that has frost's error line in it, so anything may
// both precede and follow the object.

/**
 * One entry of the `configurations` array frost emits.
 *
 * The three named fields are the ones VS Code requires of every debug
 * configuration; everything else is flavor-specific (`program`, `mainClass`,
 * `MIMode`, …) and is deliberately left as an open index signature rather than
 * a union of the five shapes frost can produce. This module never interprets
 * those fields — it passes the object to a debugger extension that does — and
 * a union here would mean a new frost flavor stops parsing rather than merely
 * being unknown.
 */
export interface LaunchConfiguration {
  name: string;
  type: string;
  request: string;
  [key: string]: unknown;
}

/** The two files `frost ide --dry-run` prints, as an object keyed by filename. */
export interface IdeFiles {
  launch: { version: string; configurations: LaunchConfiguration[] };
  /**
   * The `tasks.json` value, verbatim and uninterpreted. `tasks.ts` owns the
   * task side; keeping the payload as `unknown` means this module does not
   * acquire a second reason to change.
   */
  tasks?: unknown;
}

/** `frost: error: <message>`, the shape `main.rs` prints every failure in. */
const FROST_ERROR = /^frost:[ \t]+error:[ \t]*(.*)$/;

/** The debug types frost emits, mapped to the extension that registers each. */
const DEBUG_TYPE_EXTENSIONS: Record<string, { id: string; name: string }> = {
  cppdbg: { id: 'ms-vscode.cpptools', name: 'C/C++' },
  java: { id: 'vscjava.vscode-java-debug', name: 'Debugger for Java' },
  debugpy: { id: 'ms-python.debugpy', name: 'Python Debugger' },
  // `node` is absent on purpose — see `requiredExtension`.
};

/** Whether a JSON value is an object we can read keys off. */
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * Offsets of every `{` that begins a line, in order.
 *
 * That is the anchor for finding frost's JSON in output it shares with prose.
 * It works because the payload is printed with `serde_json::to_string_pretty`:
 * only the outermost `{` is unindented, every nested one carries at least two
 * spaces. Column 0 is required rather than "first non-space", precisely so the
 * nested objects are not candidates.
 *
 * A `\n` is the only line start tested, which also covers CRLF — the `{` still
 * follows the `\n`, with the `\r` on the previous line.
 */
function* objectStarts(text: string): Generator<number> {
  let index = text.indexOf('{');
  while (index !== -1) {
    if (index === 0 || text[index - 1] === '\n') {
      yield index;
    }
    index = text.indexOf('{', index + 1);
  }
}

/**
 * Index just past the `}` that closes the object beginning at `start`.
 *
 * Needed because the JSON is not necessarily the last thing in the text: when
 * the caller passes the stdout/stderr concatenation, a late-flushed warning can
 * land after the object, and `JSON.parse` of "object plus trailing prose"
 * throws. Slicing to the matching brace parses what frost printed and ignores
 * what surrounded it.
 *
 * String state is tracked rather than braces alone. Every path frost emits
 * contains `${workspaceFolder}`, whose braces happen to balance, so a plain
 * depth counter survives the common case by luck and not by construction — one
 * unpaired brace inside any string (a directory named with one, a shell
 * fragment in a task's argv) shifts the depth and closes the object early,
 * leaving a truncated document that fails to parse. Tracking strings costs a
 * boolean and removes the luck. The escape flag is part of that: `\"` inside a
 * string must not be read as the string's end, or everything after it is
 * scanned as if it were structure.
 *
 * Returns undefined for an unterminated object (output cut short by a killed
 * process, most likely), which the caller treats as "not the JSON".
 */
function objectEnd(text: string, start: number): number | undefined {
  let depth = 0;
  let inString = false;
  let escaped = false;
  for (let index = start; index < text.length; index += 1) {
    const character = text[index];
    if (inString) {
      if (escaped) {
        escaped = false;
      } else if (character === '\\') {
        escaped = true;
      } else if (character === '"') {
        inString = false;
      }
      continue;
    }
    if (character === '"') {
      inString = true;
    } else if (character === '{') {
      depth += 1;
    } else if (character === '}') {
      depth -= 1;
      if (depth === 0) {
        return index + 1;
      }
    }
  }
  return undefined;
}

/**
 * Convert a parsed JSON value into `IdeFiles`, or undefined if it is not one.
 *
 * The shape check is what lets `parseIdeOutput` try more than one candidate
 * offset without the risk of returning some other object that happened to
 * parse. `launch.json` with an array of configurations is the minimum that
 * makes the payload useful.
 *
 * `version` falls back to `''` when frost did not print a string one, instead
 * of rejecting the payload: the version belongs to the launch.json file format
 * and is ignored entirely when a configuration is started directly, so losing a
 * whole working debug configuration over it would be a bad trade.
 */
function toIdeFiles(parsed: unknown): IdeFiles | undefined {
  if (!isRecord(parsed)) {
    return undefined;
  }
  const root: Record<string, unknown> = parsed;
  const launch = root['launch.json'];
  if (!isRecord(launch)) {
    return undefined;
  }
  const configurations = launch['configurations'];
  if (!Array.isArray(configurations)) {
    return undefined;
  }
  const version = launch['version'];
  const files: IdeFiles = {
    launch: {
      version: typeof version === 'string' ? version : '',
      // Non-objects are dropped here because they cannot be a configuration
      // under any reading, and keeping them would make the array's element type
      // a claim this function has not checked. What is kept is still only
      // *shaped* like a configuration — `firstLaunchConfiguration` is where the
      // required fields are verified, right before anything reaches VS Code.
      configurations: configurations.filter(isRecord) as LaunchConfiguration[],
    },
  };
  if (Object.hasOwn(root, 'tasks.json')) {
    files.tasks = root['tasks.json'];
  }
  return files;
}

/**
 * Parse the output of `frost ide <target> --dry-run`.
 *
 * Undefined rather than a throw for anything unparseable, because every caller
 * of this is a command handler: showing "frost printed no launch configuration"
 * next to the raw output is strictly more useful to the person debugging than a
 * `SyntaxError` stack in the extension host log. It also means the common
 * failure — frost erroring out before it printed any JSON — flows straight into
 * `frostErrorMessage`, which has frost's own guidance in it.
 *
 * Each line-initial `{` is tried in turn rather than only the first, so a build
 * line that happens to begin with a brace costs nothing; the shape check in
 * `toIdeFiles` decides which candidate was really the payload.
 */
export function parseIdeOutput(text: string): IdeFiles | undefined {
  for (const start of objectStarts(text)) {
    const end = objectEnd(text, start);
    if (end === undefined) {
      continue;
    }
    let parsed: unknown;
    try {
      parsed = JSON.parse(text.slice(start, end));
    } catch {
      continue;
    }
    const files = toIdeFiles(parsed);
    if (files !== undefined) {
      return files;
    }
  }
  return undefined;
}

/**
 * The configuration to start, or undefined when there is none to start.
 *
 * frost emits exactly one configuration per target, so "first" is "the one".
 * It is not a search for the first *valid* entry: if frost ever emitted several
 * and the first were malformed, silently debugging the second would launch
 * something the user did not ask for.
 *
 * `name`, `type` and `request` are checked because VS Code rejects a
 * configuration missing any of them, and it does so from deep inside
 * `startDebugging` with a message that says nothing about frost. Failing here
 * lets the caller say which target produced the bad configuration.
 */
export function firstLaunchConfiguration(
  files: IdeFiles,
): LaunchConfiguration | undefined {
  const first = files.launch.configurations[0];
  if (first === undefined) {
    return undefined;
  }
  // Read through a `Record` view rather than the declared fields: the declared
  // types come from `toIdeFiles`, which proved only that this is an object.
  // Checking `first.type` directly would be a check the compiler believes is
  // already true, which is exactly how such a check gets deleted later.
  const raw: Record<string, unknown> = first;
  for (const key of ['name', 'type', 'request']) {
    const value = raw[key];
    if (typeof value !== 'string' || value.trim() === '') {
      return undefined;
    }
  }
  return first;
}

/**
 * The extension that provides a debug type, when one has to be installed.
 *
 * VS Code's failure mode for a missing debug adapter is a modal about an
 * unknown configuration type, which tells the user nothing about what to
 * install. Naming the extension turns that into one action.
 *
 * `node` returns undefined because the JavaScript debugger (`js-debug`) ships
 * *inside* VS Code — there is nothing for the user to install and no
 * marketplace id that would be right to show. That is why this returns an
 * optional entry rather than an id-or-empty-string: "built in" and "provided by
 * an extension" are genuinely different answers, and inventing an id for the
 * first would send people to a page that does not exist. An unrecognized type
 * is undefined for the opposite reason — we do not know, and guessing an id
 * from the type name would be worse than saying nothing.
 */
export function requiredExtension(
  type: string,
): { id: string; name: string } | undefined {
  // `Object.hasOwn`, not `in`: a debug type spelled `constructor` or
  // `toString` would otherwise find an inherited member and be reported as a
  // known extension.
  if (!Object.hasOwn(DEBUG_TYPE_EXTENSIONS, type)) {
    return undefined;
  }
  const entry = DEBUG_TYPE_EXTENSIONS[type];
  // A copy, so a caller that decorates the result for display cannot edit the
  // table every later lookup reads from.
  return { id: entry.id, name: entry.name };
}

/**
 * The message from frost's own `frost: error: <message>` line, if there is one.
 *
 * Worth extracting rather than reporting a generic failure, because frost's
 * errors for this command are instructions: "target ... is not compiled with
 * debug symbols in profile "debug"; add [profile.debug] cflags = ["-O0", "-g"]"
 * tells the user exactly what to edit, and "a wheel has no direct IDE launch
 * configuration; choose a runnable target" tells them the target was the wrong
 * kind. Replacing either with "frost ide failed (1)" throws away the only part
 * of the output that helps.
 *
 * The prefix is stripped so the caller can put the message in an editor
 * notification, where a second "frost: error:" in front of it reads as noise.
 *
 * The first error wins: frost prints the cause first and any consequences after
 * it, so the first line is the one to act on.
 */
export function frostErrorMessage(text: string): string | undefined {
  for (const rawLine of text.split('\n')) {
    // Full trim, which also disposes of the `\r` of CRLF output; left on, it
    // would ride to the end of every extracted message and show up as a stray
    // box glyph in the notification.
    const match = FROST_ERROR.exec(rawLine.trim());
    if (match === null) {
      continue;
    }
    const message = (match[1] as string).trim();
    if (message === '') {
      // `frost: error:` with nothing after it is not guidance; keep looking
      // rather than reporting an empty notification.
      continue;
    }
    return message;
  }
  return undefined;
}
