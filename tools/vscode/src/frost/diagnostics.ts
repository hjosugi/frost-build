// Reading diagnostics out of a build's console output.
//
// docs/28_compatibility_contract.md promises the `--json` surfaces; the human
// progress text is explicitly not a promise. So everything here is a
// best-effort read of prose, written to fail quiet: a line this module does not
// recognise is dropped rather than guessed at. A missed warning costs a
// squiggle nobody sees; an invented one sends someone to a file that is fine.
//
// Nothing here imports `vscode` or spawns anything — it takes text that
// `cli.ts` already captured. That is what lets the parser be exercised in CI
// without downloading a VS Code build, and this file is where nearly all of the
// extension's guessable logic lives.

import type {
  BuildOutcome,
  DiagnosticSeverity,
  FrostDiagnostic,
} from './types';

/**
 * A compiler diagnostic in the GCC/Clang long form.
 *
 * The file part is lazy on purpose. A greedy match would swallow the line and
 * column into the path; a lazy one stops at the first `:` that is followed by
 * digits, which is also what makes Windows drive letters work — `C` is not
 * followed by a number in `C:\src\a.c:13:17:`, so the match slides past it.
 *
 * The column group is optional because plenty of tools (and GCC itself, for
 * some whole-file diagnostics) report only a line, and dropping those would
 * lose exactly the diagnostics that are hardest to find by hand.
 *
 * `fatal error` leads the severity alternation so it is preferred over the
 * `error` that it contains.
 */
const DIAGNOSTIC =
  /^(.+?):(\d+)(?::(\d+))?:[ \t]+(fatal error|error|warning|note):[ \t]+(.*)$/;

/** `[3/9] CC render/src/render.c (//render:render)`. */
const PROGRESS = /^\[\d+\/\d+\][ \t]+(.*)$/;

/** `FAILED: CC core/src/core.c (//core:core)`. */
const FAILED = /^FAILED:[ \t]+(.*)$/;

/**
 * The label frost appends to an action description.
 *
 * Deliberately not `\(([^)]*)\)$`: several descriptions end in a parenthesised
 * something that is not a target — `TEST slow (shard 1/2)` is the one that bites
 * — so this only accepts text shaped like a label, either `//pkg:name` or the
 * bare name a root target gets. Target names are `[A-Za-z0-9_-]+` per
 * `manifest.rs`, which is what rules `shard 1/2` out.
 */
const TARGET_IN_DESC = /\((\/\/[^\s()]*:[A-Za-z0-9_-]+|[A-Za-z0-9_-]+)\)[ \t]*$/;

/**
 * The source echo GCC and Clang print under a diagnostic:
 *
 *     13 |     return a + b
 *        |                 ^
 *
 * Skipping these is load-bearing, not tidiness. Echoed source is arbitrary
 * text, and a line of C like `foo("a:1: error: x")` parses as a perfect
 * diagnostic pointing at a file named `13 |     foo("a`. The `|` gutter is the
 * only reliable way to tell echoed source from a real diagnostic, so it is
 * checked before anything else.
 */
const SOURCE_ECHO = /^[ \t]*\d*[ \t]*\|/;

/** frost's own framing of a failed command; neither line locates anything. */
const COMMAND_OR_EXIT = /^[ \t]*(?:command|exit):[ \t]/;

/**
 * `failure summary (first 10):`.
 *
 * Matched loosely on the leading words rather than the exact count, since the
 * "first N" cap is a constant in the CLI and a change to it should not silently
 * cost us the whole block.
 */
const FAILURE_SUMMARY_HEADER = /^failure summary\b/;

/** The final `frost: ...` line every run ends with. */
const SUMMARY = /^frost:[ \t]/;

/**
 * Normalize a path a compiler reported into the form the rest of the extension
 * expects: `/`-separated, no `./` noise.
 *
 * Absolute paths need no special case — neither operation can turn one into
 * something else — but they are deliberately left absolute rather than made
 * relative here. Only the VS Code layer knows the workspace root, and a path
 * outside the workspace (a system header, a generated file under a temp dir)
 * has no relative form at all.
 */
export function normalizeDiagnosticPath(path: string): string {
  let normalized = path.replace(/\\/g, '/');
  // Repeated rather than a single strip: `./` prefixes stack when a build
  // system passes `-I.` down, and `../` is left alone because it means
  // something.
  while (normalized.startsWith('./')) {
    normalized = normalized.slice(2);
  }
  return normalized;
}

/** `note` is advisory context, which is what an editor calls 'info'. */
function toSeverity(word: string): DiagnosticSeverity {
  switch (word) {
    case 'note':
      return 'info';
    case 'warning':
      return 'warning';
    default:
      // 'error' and 'fatal error'. The distinction matters to the compiler
      // (whether it kept going), not to a squiggle.
      return 'error';
  }
}

/**
 * The target an action description names, or `undefined` when it names none.
 *
 * `LINK cli`, `AR libcore.a` and `GEN gen_version` carry no label, and the
 * target could only be guessed from them. Undefined is the honest answer:
 * a diagnostic with no target still shows up in the Problems panel, whereas one
 * filed under the wrong target sends someone to the wrong `frost.toml`.
 */
function targetOf(description: string): string | undefined {
  const match = TARGET_IN_DESC.exec(description);
  return match ? match[1] : undefined;
}

/**
 * Pull the action id off one line of the failure summary block.
 *
 * The block's entries are `  <action-id>: <first line of detail>`, and the
 * action ids contain colons themselves — `compile:<target>:<source>` expands to
 * `compile://core:core:core/src/core.c`. Splitting on the first `:` or the last
 * `:` both give garbage. The separator is the first `": "`, colon-space, which
 * an id cannot contain: ids are built from labels and source paths, and neither
 * has a space after a colon.
 *
 * A failure whose detail is empty prints as `  <id>: ` with a trailing space,
 * so the search runs against the untrimmed line and still finds its separator.
 */
function failureEntryId(line: string): string | undefined {
  if (!line.startsWith('  ')) {
    return undefined;
  }
  const body = line.slice(2);
  const separator = body.indexOf(': ');
  const id = separator === -1 ? body.replace(/:$/, '') : body.slice(0, separator);
  return id === '' ? undefined : id;
}

/**
 * Parse the output of `frost build --no-tui` (or `frost test`).
 *
 * The reason this exists rather than a VS Code `problemMatcher`: a matcher sees
 * a flat stream of lines and can only pattern-match each one in isolation. It
 * cannot see frost's action framing — the `FAILED: CC core/src/core.c
 * (//core:core)` and `[3/9] ...` lines that say which target's action produced
 * the diagnostics that follow. Attribution is the whole point; it is what lets
 * the extension offer "rebuild just this target" from a squiggle, and a matcher
 * structurally cannot produce it.
 *
 * Both kinds of framing line update the attribution, not just `FAILED:`.
 * Successful actions print their captured output (warnings, mostly) under their
 * progress line, so treating only `FAILED:` as a boundary would file every
 * later warning in the build under the last target that happened to fail.
 *
 * `tests` is left undefined here on purpose; the `tests:` line has its own
 * parser.
 */
export function parseBuildOutput(text: string): BuildOutcome {
  const diagnostics: FrostDiagnostic[] = [];
  const failedActions: string[] = [];
  // A compiler repeats a note per instantiation or per include path, and the
  // Problems panel shows every copy. Identical means identical location,
  // severity and text; the target is not part of the key, so the same header
  // error reported once per dependent library collapses to the first target
  // that hit it rather than stacking up.
  const seen = new Set<string>();
  let summary: string | undefined;
  let target: string | undefined;
  let inFailureSummary = false;

  // Split on all three terminators: the output may have come from a Windows
  // toolchain even when the extension is not running on Windows.
  for (const line of text.split(/\r\n|\n|\r/)) {
    if (inFailureSummary) {
      const id = failureEntryId(line);
      if (id !== undefined) {
        failedActions.push(id);
        continue;
      }
      // The block ends at the first unindented line — in a test run that is the
      // `tests:` line, which must not be mistaken for an action id. Fall
      // through so this line is still read as ordinary output.
      inFailureSummary = false;
    }

    if (FAILURE_SUMMARY_HEADER.test(line)) {
      inFailureSummary = true;
      continue;
    }

    const failed = FAILED.exec(line);
    if (failed) {
      target = targetOf(failed[1] as string);
      continue;
    }

    const progress = PROGRESS.exec(line);
    if (progress) {
      target = targetOf(progress[1] as string);
      continue;
    }

    if (SUMMARY.test(line)) {
      // Verbatim, and last one wins: `frost watch` prints a banner in the same
      // shape before the build it is about, and the summary is what the run
      // ended with.
      summary = line;
      continue;
    }

    if (SOURCE_ECHO.test(line) || COMMAND_OR_EXIT.test(line)) {
      continue;
    }

    // Leading whitespace is tolerated because GCC indents the notes it hangs
    // off a diagnostic, and those carry the explanation people actually need.
    const match = DIAGNOSTIC.exec(line.replace(/^[ \t]+/, ''));
    if (!match) {
      continue;
    }
    const message = (match[5] as string).replace(/[ \t]+$/, '');
    if (message === '') {
      // A located but wordless diagnostic is a squiggle with nothing to say.
      continue;
    }
    const file = normalizeDiagnosticPath(match[1] as string);
    const lineNumber = Number(match[2]);
    const column = match[3] === undefined ? undefined : Number(match[3]);
    const severity = toSeverity(match[4] as string);

    const key = `${file}\u0000${lineNumber}\u0000${column ?? ''}\u0000${severity}\u0000${message}`;
    if (seen.has(key)) {
      continue;
    }
    seen.add(key);

    // Optional fields are omitted rather than set to `undefined` so the shape
    // survives `exactOptionalPropertyTypes` and round-trips through JSON.
    const diagnostic: FrostDiagnostic = {
      file,
      line: lineNumber,
      severity,
      message,
    };
    if (column !== undefined) {
      diagnostic.column = column;
    }
    if (target !== undefined) {
      diagnostic.target = target;
    }
    diagnostics.push(diagnostic);
  }

  const outcome: BuildOutcome = { diagnostics, failedActions };
  if (summary !== undefined) {
    outcome.summary = summary;
  }
  return outcome;
}
