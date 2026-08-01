// Everything here runs against captured text, never against a live frost, so
// the fixtures are the contract: they are copied verbatim from real runs of
// `frost build --no-tui` and should only be edited by re-capturing.

import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  normalizeDiagnosticPath,
  parseBuildOutput,
} from '../src/frost/diagnostics';
import type { FrostDiagnostic } from '../src/frost/types';

/** `frost build --no-tui` on a workspace with a syntax error in core.c. */
const SYNTAX_ERROR_BUILD = [
  '[1/9] GEN gen_version',
  'FAILED: CC core/src/core.c (//core:core)',
  'command: cc -O2 -Wall -Icore/include -Igen -MD -MF .frost/obj/debug/core_core/core/src/core.c.o.d -c core/src/core.c -o .frost/obj/debug/core_core/core/src/core.c.o',
  'exit: code 1',
  "core/src/core.c: In function 'frost_core_add':",
  "core/src/core.c:13:17: error: expected ';' before '}' token",
  '   13 |     return a + b',
  '      |                 ^',
  '      |                 ;',
  '   14 | }',
  '      | ~',
  '[3/9] CC render/src/render.c (//render:render)',
  '[4/9] CC apps/cli/src/main.c (//apps/cli:cli)',
  'frost: 1 failed, 4 built, 4 skipped · 9 of 12 actions · 52 ms',
  'failure summary (first 10):',
  '  compile://core:core:core/src/core.c: command: cc -O2 -Wall -Icore/include ...',
  '',
].join('\n');

const SUCCESSFUL_BUILD = [
  '[1/9] GEN gen_version',
  'frost: 9 built · 9 of 12 actions · 71 ms',
  '',
].join('\n');

const WARM_BUILD = 'frost: up to date · 9 of 12 actions · 1 ms\n';

test('the captured syntax-error build yields exactly the diagnostic the compiler reported', () => {
  const outcome = parseBuildOutput(SYNTAX_ERROR_BUILD);

  // Exactness matters more than the individual fields: this fixture has eleven
  // lines that a careless pattern would also turn into diagnostics (the caret
  // art, the echoed source, the command line, the progress lines).
  assert.equal(outcome.diagnostics.length, 1);
  assert.deepStrictEqual(outcome.diagnostics[0], {
    file: 'core/src/core.c',
    line: 13,
    column: 17,
    severity: 'error',
    message: "expected ';' before '}' token",
    target: '//core:core',
  } satisfies FrostDiagnostic);
});

test("the 'In function' banner does not become a diagnostic", () => {
  const outcome = parseBuildOutput(SYNTAX_ERROR_BUILD);

  // `core/src/core.c: In function 'frost_core_add':` is shaped like a
  // diagnostic minus the line number and severity. Anything that keys off "a
  // path followed by a colon" turns it into a phantom error on line 1, which is
  // the single most common way this kind of parser goes wrong.
  const banners = outcome.diagnostics.filter((d) =>
    d.message.includes('In function'),
  );
  assert.deepStrictEqual(banners, []);
});

test('a failure summary entry keeps every colon inside the action id', () => {
  const outcome = parseBuildOutput(SYNTAX_ERROR_BUILD);

  // The regression this catches: splitting `  <id>: <detail>` on the first `:`
  // yields `compile`, splitting on the last yields the id minus the source
  // file. Only the first colon-space is the separator, because an action id
  // (`compile:<label>:<source>`) never contains one.
  assert.deepStrictEqual(outcome.failedActions, [
    'compile://core:core:core/src/core.c',
  ]);
});

test('the frost summary line is captured verbatim', () => {
  const outcome = parseBuildOutput(SYNTAX_ERROR_BUILD);

  assert.equal(
    outcome.summary,
    'frost: 1 failed, 4 built, 4 skipped · 9 of 12 actions · 52 ms',
  );
});

test('a successful build reports no diagnostics and no failures', () => {
  const outcome = parseBuildOutput(SUCCESSFUL_BUILD);

  assert.deepStrictEqual(outcome.diagnostics, []);
  assert.deepStrictEqual(outcome.failedActions, []);
  assert.equal(outcome.summary, 'frost: 9 built · 9 of 12 actions · 71 ms');
});

test('a warm build reports no diagnostics and the up-to-date summary', () => {
  const outcome = parseBuildOutput(WARM_BUILD);

  // The warm path is the one developers hit all day, so the parser has to stay
  // silent on it: a stray diagnostic here would repaint the Problems panel on
  // every save.
  assert.deepStrictEqual(outcome.diagnostics, []);
  assert.equal(outcome.summary, 'frost: up to date · 9 of 12 actions · 1 ms');
});

test('a diagnostic without a column parses with column undefined', () => {
  const outcome = parseBuildOutput("foo.c:7: warning: unused variable 'x'\n");

  assert.equal(outcome.diagnostics.length, 1);
  const diagnostic = outcome.diagnostics[0] as FrostDiagnostic;
  assert.equal(diagnostic.file, 'foo.c');
  assert.equal(diagnostic.line, 7);
  // Not 0 and not 1: an invented column moves the squiggle off the token the
  // compiler meant, and VS Code cannot tell that the number was a guess.
  assert.equal(diagnostic.column, undefined);
  assert.equal(diagnostic.severity, 'warning');
  assert.equal(diagnostic.message, "unused variable 'x'");
});

test('note maps to info and fatal error maps to error', () => {
  const outcome = parseBuildOutput(
    [
      "core/include/core.h:4:9: note: previous declaration of 'frost_core_add' was here",
      "apps/cli/src/main.c:1:10: fatal error: missing.h: No such file or directory",
      '',
    ].join('\n'),
  );

  assert.equal(outcome.diagnostics.length, 2);
  assert.equal(outcome.diagnostics[0]?.severity, 'info');
  // `fatal error` has to be tried before `error` in the severity alternation,
  // or the leading `fatal ` ends up glued onto the file path.
  assert.equal(outcome.diagnostics[1]?.severity, 'error');
  assert.equal(outcome.diagnostics[1]?.file, 'apps/cli/src/main.c');
  assert.equal(
    outcome.diagnostics[1]?.message,
    'missing.h: No such file or directory',
  );
});

test('identical diagnostics collapse to the first occurrence', () => {
  const outcome = parseBuildOutput(
    [
      "core/include/core.h:9:5: note: in expansion of macro 'FROST_CHECK'",
      "core/include/core.h:9:5: note: in expansion of macro 'FROST_CHECK'",
      // Padded by whatever captured the log. The message has to be trimmed
      // before it becomes a dedup key or a repeat sneaks through looking
      // different, and the Problems panel shows the same note twice.
      "core/include/core.h:9:5: note: in expansion of macro 'FROST_CHECK'   ",
      // Same text, different line: must survive, or dedup is too aggressive.
      "core/include/core.h:11:5: note: in expansion of macro 'FROST_CHECK'",
      '',
    ].join('\n'),
  );

  assert.equal(outcome.diagnostics.length, 2);
  assert.equal(outcome.diagnostics[0]?.line, 9);
  assert.equal(
    outcome.diagnostics[0]?.message,
    "in expansion of macro 'FROST_CHECK'",
  );
  assert.equal(outcome.diagnostics[1]?.line, 11);
});

test('the echoed command line never becomes a diagnostic', () => {
  const outcome = parseBuildOutput(
    [
      'FAILED: CC a.c (//a:a)',
      // A command line is arbitrary text, and this one contains a perfectly
      // well-formed diagnostic inside a -D value. Only the `command:` prefix
      // tells us not to read it.
      'command: cc -c a.c -o a.o -DBANNER="1:2: error: not a real problem"',
      'exit: code 1',
      '',
    ].join('\n'),
  );

  assert.deepStrictEqual(outcome.diagnostics, []);
});

test('echoed source lines never become diagnostics', () => {
  const outcome = parseBuildOutput(
    [
      'FAILED: CC a.c (//a:a)',
      "a.c:9:5: error: expected ')' before ';' token",
      // GCC echoes the offending source under the diagnostic. The source here
      // is a string literal that itself looks like a diagnostic; without the
      // `|` gutter check this parses as an error in a file called
      // `    9 |     log("x`.
      '    9 |     log("x:1: error: boom");',
      '      |     ^',
      '',
    ].join('\n'),
  );

  assert.equal(outcome.diagnostics.length, 1);
  assert.equal(outcome.diagnostics[0]?.file, 'a.c');
  assert.equal(outcome.diagnostics[0]?.line, 9);
});

test('a located but wordless diagnostic is dropped', () => {
  const outcome = parseBuildOutput(
    ['a.c:1:1: error: ', "a.c:2:1: error: real problem", ''].join('\n'),
  );

  // A squiggle with nothing to say is worse than no squiggle: it marks code as
  // broken and gives the reader no way to find out why.
  assert.equal(outcome.diagnostics.length, 1);
  assert.equal(outcome.diagnostics[0]?.line, 2);
});

test('attribution follows frost action framing instead of sticking to the last failure', () => {
  const outcome = parseBuildOutput(
    [
      'FAILED: CC core/src/core.c (//core:core)',
      "core/src/core.c:13:17: error: expected ';' before '}' token",
      // A successful action prints its captured warnings under its progress
      // line. Treating only FAILED: as a boundary files every warning in the
      // rest of the build under //core:core, which is the regression here.
      '[3/9] CC render/src/render.c (//render:render)',
      "render/src/render.c:4:9: warning: unused variable 'tmp'",
      // GEN carries no label, so there is nothing to attribute to and the
      // parser must say so rather than reuse //render:render.
      '[4/9] GEN gen_version',
      'gen/version.h:2:1: warning: regenerated header is not guarded',
      '',
    ].join('\n'),
  );

  assert.equal(outcome.diagnostics.length, 3);
  assert.equal(outcome.diagnostics[0]?.target, '//core:core');
  assert.equal(outcome.diagnostics[1]?.target, '//render:render');
  assert.equal(outcome.diagnostics[2]?.target, undefined);
});

test('a parenthesised shard suffix is not mistaken for a target', () => {
  const outcome = parseBuildOutput(
    [
      // The description of a sharded test ends in `(shard 1/2)`. A naive
      // `\(([^)]*)\)$` reports `shard 1/2` as the target and the extension then
      // tries to build it.
      'FAILED: TEST slow (shard 1/2)',
      'tests/slow.c:3:1: error: assertion failed',
      '',
    ].join('\n'),
  );

  assert.equal(outcome.diagnostics.length, 1);
  assert.equal(outcome.diagnostics[0]?.target, undefined);
});

test('a root target is attributed by its bare name', () => {
  const outcome = parseBuildOutput(
    [
      'FAILED: KOFUN app/Main.kt (app)',
      'app/Main.kt:12:3: error: unresolved reference',
      '',
    ].join('\n'),
  );

  assert.equal(outcome.diagnostics[0]?.target, 'app');
});

test('the failure summary block ends at the first unindented line', () => {
  // A watch session puts several builds in one stream, so the block has to
  // close rather than run to the end of the output.
  const outcome = parseBuildOutput(
    [
      'frost: 1 failed, 3 built · 4 of 4 actions · 12 ms',
      'failure summary (first 10):',
      '  test:unit: exit: code 1',
      // Printed straight after the block on a test run, and unindented, which
      // is what closes the block.
      'tests: 3 passed, 1 failed, 0 cached',
      '[1/4] CC a.c (//a:a)',
      "a.c:2:5: warning: unused variable 'y'",
      // GCC indents the note it hangs off a warning. If the block is still open
      // this is read as an action id of `a.c:2:5` and the note is lost — the
      // regression that a "block runs to EOF" implementation causes.
      '  a.c:2:5: note: declared here',
      '',
    ].join('\n'),
  );

  assert.deepStrictEqual(outcome.failedActions, ['test:unit']);
  assert.equal(outcome.diagnostics.length, 2);
  assert.equal(outcome.diagnostics[1]?.severity, 'info');
  assert.equal(outcome.diagnostics[1]?.message, 'declared here');
  assert.equal(outcome.diagnostics[1]?.target, '//a:a');
  // The indent belongs to GCC's presentation, not to the path. Leaving it on
  // gives VS Code a file name it will never resolve, and the note lands nowhere.
  assert.equal(outcome.diagnostics[1]?.file, 'a.c');
  // Test counts are parsed elsewhere; this module must not half-fill them.
  assert.equal(outcome.tests, undefined);
});

test('the last frost: line is the summary', () => {
  // `frost watch` prints a banner in the same shape before the build it is
  // about, so taking the first match reports the banner as the build result and
  // the status bar never updates.
  const outcome = parseBuildOutput(
    [
      'frost: watch · profile debug · platform host · debounce 40 ms',
      '[1/9] GEN gen_version',
      'frost: 9 built · 9 of 12 actions · 71 ms',
      '',
    ].join('\n'),
  );

  assert.equal(outcome.summary, 'frost: 9 built · 9 of 12 actions · 71 ms');
});

test('CRLF output parses identically to LF output', () => {
  // Captured from a Windows toolchain, or from any tool that writes CRLF while
  // the extension runs elsewhere. A leaked `\r` shows up as a trailing box in
  // the Problems panel and silently breaks action-id comparisons.
  const outcome = parseBuildOutput(SYNTAX_ERROR_BUILD.replace(/\n/g, '\r\n'));

  assert.deepStrictEqual(outcome, parseBuildOutput(SYNTAX_ERROR_BUILD));
});

test('empty output produces an empty outcome with no summary key', () => {
  const outcome = parseBuildOutput('');

  assert.deepStrictEqual(outcome.diagnostics, []);
  assert.deepStrictEqual(outcome.failedActions, []);
  // Absent rather than present-and-undefined, so the outcome survives a JSON
  // round trip through the extension host unchanged.
  assert.equal('summary' in outcome, false);
});

test('normalizeDiagnosticPath converts Windows separators', () => {
  assert.equal(
    normalizeDiagnosticPath('core\\src\\core.c'),
    'core/src/core.c',
  );
  // Drive-qualified paths stay absolute: only the VS Code layer knows the
  // workspace root, and a system header has no relative form at all.
  assert.equal(normalizeDiagnosticPath('C:\\src\\a.c'), 'C:/src/a.c');
});

test('normalizeDiagnosticPath strips leading ./ without touching ../', () => {
  assert.equal(normalizeDiagnosticPath('./core/src/core.c'), 'core/src/core.c');
  // `-I.` chains stack these up.
  assert.equal(normalizeDiagnosticPath('././a.c'), 'a.c');
  assert.equal(normalizeDiagnosticPath('.\\a.c'), 'a.c');
  // `..` is meaningful; stripping it would silently point at a different file.
  assert.equal(normalizeDiagnosticPath('../shared/a.c'), '../shared/a.c');
});

test('normalizeDiagnosticPath leaves already-normal and absolute paths alone', () => {
  assert.equal(normalizeDiagnosticPath('core/src/core.c'), 'core/src/core.c');
  assert.equal(
    normalizeDiagnosticPath('/usr/include/stdio.h'),
    '/usr/include/stdio.h',
  );
});

test('diagnostic file paths are normalized on the way out', () => {
  const outcome = parseBuildOutput('.\\core\\src\\core.c:3:1: error: boom\n');

  // Normalization has to happen inside the parser, not at the VS Code
  // boundary, or every consumer has to remember to do it.
  assert.equal(outcome.diagnostics[0]?.file, 'core/src/core.c');
});
