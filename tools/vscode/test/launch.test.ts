// Tests for the `frost ide --dry-run` reader. `node:test` and
// `node:assert/strict` only — the module never imports `vscode`, so none of
// this needs a downloaded editor, and none of it needs a built frost either:
// the fixtures are captured output.

import * as assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  firstLaunchConfiguration,
  frostErrorMessage,
  parseIdeOutput,
  requiredExtension,
  type IdeFiles,
} from '../src/frost/launch';

/**
 * Verbatim `frost -C sample_multi ide //apps/cli:cli --dry-run` stdout.
 *
 * Two things about it are the reason this module exists. The first line is a
 * build summary, because `ide` builds before it prints — so the payload is not
 * the whole of stdout. And the object's keys are alphabetical, which is what
 * `serde_json`'s map ordering produces; nothing here should depend on that, and
 * the fixture keeps it so a parser that quietly did would be caught.
 *
 * The `tasks.json` block is the only reconstructed part (the capture elided its
 * body, and a fixture has to be parseable). It is written to match what
 * `vscode_files` in `crates/frostbuild-cli/src/main.rs` emits.
 */
const CPPDBG_OUTPUT = `frost: up to date · 9 of 12 actions · 1 ms
{
  "launch.json": {
    "configurations": [
      {
        "MIMode": "gdb",
        "args": [],
        "cwd": "\${workspaceFolder}",
        "name": "Frost: debug //apps/cli:cli",
        "preLaunchTask": "frost: build //apps/cli:cli",
        "program": "\${workspaceFolder}/.frost/bin/debug/apps_cli_cli",
        "request": "launch",
        "stopAtEntry": false,
        "type": "cppdbg"
      }
    ],
    "version": "0.2.0"
  },
  "tasks.json": {
    "tasks": [
      {
        "args": [
          "-C",
          "\${workspaceFolder}",
          "build",
          "//apps/cli:cli",
          "--profile",
          "debug",
          "--platform",
          "",
          "--no-tui"
        ],
        "command": "frost",
        "group": {
          "isDefault": true,
          "kind": "build"
        },
        "label": "frost: build //apps/cli:cli",
        "options": {
          "cwd": "\${workspaceFolder}"
        },
        "problemMatcher": [
          "$gcc"
        ],
        "type": "process"
      }
    ],
    "version": "2.0.0"
  }
}
`;

/** The `.jar` flavor, in the shape `vscode_files` builds for a JAR artifact. */
const JAVA_OUTPUT = `frost: 4 built, 8 cached · 12 actions · 613 ms
{
  "launch.json": {
    "configurations": [
      {
        "args": [],
        "classPaths": [
          "\${workspaceFolder}/.frost/bin/debug/apps_svc_svc.jar"
        ],
        "cwd": "\${workspaceFolder}",
        "mainClass": "com.example.svc.Main",
        "name": "Frost: debug //apps/svc:svc",
        "preLaunchTask": "frost: build //apps/svc:svc",
        "request": "launch",
        "type": "java"
      }
    ],
    "version": "0.2.0"
  },
  "tasks.json": {
    "tasks": [],
    "version": "2.0.0"
  }
}
`;

/**
 * The `.js` flavor. `sourceMaps` is false here on purpose: frost computes it
 * from whether any action in the closure emits a `.map`, so false is the
 * ordinary answer, and it is the value a careless parser loses.
 */
const NODE_OUTPUT = `frost: up to date · 3 actions · 2 ms
{
  "launch.json": {
    "configurations": [
      {
        "args": [],
        "cwd": "\${workspaceFolder}",
        "name": "Frost: debug //web:bundle",
        "preLaunchTask": "frost: build //web:bundle",
        "program": "\${workspaceFolder}/.frost/bin/debug/web_bundle.js",
        "request": "launch",
        "sourceMaps": false,
        "type": "node"
      }
    ],
    "version": "0.2.0"
  },
  "tasks.json": {
    "tasks": [],
    "version": "2.0.0"
  }
}
`;

/** Parse a fixture, failing the test rather than returning undefined. */
function parse(text: string): IdeFiles {
  const files = parseIdeOutput(text);
  if (files === undefined) {
    throw new Error('expected the fixture to parse');
  }
  return files;
}

/** The first configuration of a fixture, failing the test if there is none. */
function config(text: string): Record<string, unknown> {
  const configuration = firstLaunchConfiguration(parse(text));
  if (configuration === undefined) {
    throw new Error('expected the fixture to yield a launch configuration');
  }
  return configuration;
}

test('parses real output despite the build summary line in front of the JSON', () => {
  // The test this module exists for. `frost ide` builds before it prints, so
  // stdout begins with `frost: up to date · ...` and a plain JSON.parse of the
  // captured output throws on character one.
  const files = parse(CPPDBG_OUTPUT);

  assert.equal(files.launch.version, '0.2.0');
  assert.equal(files.launch.configurations.length, 1);
});

test('keeps the tasks.json payload alongside the launch configuration', () => {
  // The caller registers the build task from this; dropping it would leave
  // `preLaunchTask` pointing at a task nothing defines, and VS Code refuses to
  // start a debug session whose preLaunchTask does not resolve.
  const tasks = parse(CPPDBG_OUTPUT).tasks as { tasks: { label: string }[] };
  assert.deepEqual(
    tasks.tasks.map((entry) => entry.label),
    ['frost: build //apps/cli:cli'],
  );
});

test('firstLaunchConfiguration returns the cppdbg configuration intact', () => {
  const configuration = config(CPPDBG_OUTPUT);

  assert.equal(configuration.type, 'cppdbg');
  assert.equal(configuration.request, 'launch');
  assert.equal(configuration.name, 'Frost: debug //apps/cli:cli');
  // The two fields the debug session is actually built out of. `program` is
  // the one thing we would have had to recompute ourselves (artifact path,
  // profile directory, name mangling) had we not delegated to frost, so it is
  // asserted in full — a truncation or an unescaped `${workspaceFolder}` shows
  // up here.
  assert.equal(
    configuration.program,
    '${workspaceFolder}/.frost/bin/debug/apps_cli_cli',
  );
  assert.equal(configuration.preLaunchTask, 'frost: build //apps/cli:cli');
  // Flavor-specific fields survive the round trip; the cpptools adapter reads
  // both, and `stopAtEntry: false` is the value a truthiness-based copy drops.
  assert.equal(configuration.MIMode, 'gdb');
  assert.equal(configuration.stopAtEntry, false);
  assert.deepEqual(configuration.args, []);
});

test('a java configuration keeps mainClass and classPaths', () => {
  // vscode-java-debug launches from these two and nothing else — a parser that
  // only preserved the fields the cppdbg flavor uses would produce a
  // configuration that loads and then fails to start.
  const configuration = config(JAVA_OUTPUT);

  assert.equal(configuration.type, 'java');
  assert.equal(configuration.mainClass, 'com.example.svc.Main');
  assert.deepEqual(configuration.classPaths, [
    '${workspaceFolder}/.frost/bin/debug/apps_svc_svc.jar',
  ]);
  // No `program` for this flavor; inventing one would be a silent lie.
  assert.equal(configuration.program, undefined);
});

test('a node configuration keeps sourceMaps false', () => {
  // `sourceMaps: false` is falsy, so any `??`/`||` defaulting or truthy filter
  // in the copy path turns it into "absent" and js-debug then goes looking for
  // maps that a bundle without them does not have.
  const configuration = config(NODE_OUTPUT);

  assert.equal(configuration.type, 'node');
  assert.ok(Object.hasOwn(configuration, 'sourceMaps'));
  assert.equal(configuration.sourceMaps, false);
});

test('braces and escaped quotes inside strings do not truncate the object', () => {
  // The `${workspaceFolder}` in every real path balances, so a brace counter
  // that ignores strings passes the fixtures above by luck. These two do not
  // balance: an unpaired `}` in a path, and a `}` guarded by escaped quotes.
  // Either one shifts a naive counter's depth, which closes the object early
  // and yields a fragment that fails to parse — so the assertion is that the
  // keys *after* them survived.
  const files = parse(`frost: up to date · 2 actions · 1 ms
{
  "launch.json": {
    "configurations": [
      {
        "name": "Frost: debug \\"}\\" cli",
        "program": "\${workspaceFolder}/.frost/bin/debug/odd}name",
        "request": "launch",
        "type": "cppdbg"
      }
    ],
    "version": "0.2.0"
  },
  "tasks.json": { "tasks": [] }
}
`);

  assert.equal(files.launch.version, '0.2.0');
  assert.notEqual(files.tasks, undefined);
  const configuration = firstLaunchConfiguration(files);
  assert.equal(configuration?.name, 'Frost: debug "}" cli');
  assert.equal(
    configuration?.program,
    '${workspaceFolder}/.frost/bin/debug/odd}name',
  );
});

test('output after the JSON does not prevent parsing', () => {
  // Callers hand this the stdout/stderr concatenation, because that is the only
  // text frost's error line appears in. A warning flushed late lands after the
  // object, where `JSON.parse` of everything-from-the-brace would throw.
  const files = parse(`${CPPDBG_OUTPUT}frost: warning: stale journal entry\n`);
  assert.equal(files.launch.configurations.length, 1);
});

test('parses CRLF output identically', () => {
  // frost's stdout arrives CRLF-terminated on Windows, which moves every
  // line-initial `{` one character and would defeat a `\n{` search.
  const crlf = parseIdeOutput(CPPDBG_OUTPUT.replace(/\n/g, '\r\n'));
  assert.deepEqual(crlf, parseIdeOutput(CPPDBG_OUTPUT));
});

test('output with no JSON returns undefined instead of throwing', () => {
  // The whl case reaches us exactly like this: frost fails before printing
  // anything, and a command handler that showed a SyntaxError stack instead of
  // frost's message would be strictly less useful.
  assert.equal(
    parseIdeOutput(
      [
        'frost: error: a wheel has no direct IDE launch configuration; choose a runnable target',
        '',
      ].join('\n'),
    ),
    undefined,
  );
  assert.equal(parseIdeOutput(''), undefined);
});

test('malformed JSON returns undefined instead of throwing', () => {
  // A killed process truncates mid-document; a `{` printed by something else
  // never was a document. Neither may escape as an exception.
  assert.equal(parseIdeOutput('frost: up to date\n{ "launch.json": {'), undefined);
  assert.equal(parseIdeOutput('{\n  "launch.json": { oops }\n}\n'), undefined);
  assert.equal(parseIdeOutput('{}\n'), undefined);
  // Right key, wrong shape: `configurations` must be an array or there is
  // nothing to launch.
  assert.equal(
    parseIdeOutput('{\n  "launch.json": { "configurations": "none" }\n}\n'),
    undefined,
  );
});

test('a leading line that starts with a brace does not shadow the payload', () => {
  // Every line-initial `{` is a candidate, and the shape check picks the real
  // one — so a stray brace line does not cost the whole parse.
  const files = parse(`{not json}\n${CPPDBG_OUTPUT}`);
  assert.equal(files.launch.configurations.length, 1);
});

test('firstLaunchConfiguration rejects a configuration missing type', () => {
  // VS Code's own rejection for this is a modal about the configuration being
  // invalid, raised from inside startDebugging with no mention of frost or of
  // which target produced it. Catching it here is what lets the caller say.
  const files = parse(`{
  "launch.json": {
    "configurations": [
      { "name": "Frost: debug //apps/cli:cli", "request": "launch" }
    ],
    "version": "0.2.0"
  }
}`);

  assert.equal(files.launch.configurations.length, 1, 'the entry is still parsed');
  assert.equal(firstLaunchConfiguration(files), undefined);
});

test('firstLaunchConfiguration rejects blank required fields', () => {
  // `"type": ""` is present-but-useless, and an existence-only check passes it
  // straight through to a debug session that cannot resolve an adapter.
  const files = parse(`{
  "launch.json": {
    "configurations": [
      { "name": "Frost", "request": "launch", "type": "  " }
    ],
    "version": "0.2.0"
  }
}`);
  assert.equal(firstLaunchConfiguration(files), undefined);
});

test('firstLaunchConfiguration returns undefined for an empty configurations array', () => {
  const files = parse('{ "launch.json": { "configurations": [], "version": "0.2.0" } }');
  assert.equal(firstLaunchConfiguration(files), undefined);
});

test('a configurations entry that is not an object is dropped', () => {
  // Nothing frost emits looks like this, but the array's element type is a
  // claim the parser makes about JSON it did not write. Keeping a number in
  // there would make that claim false for every caller downstream.
  const files = parse(`{
  "launch.json": {
    "configurations": [42, null, ["nested"]],
    "version": "0.2.0"
  }
}`);
  assert.deepEqual(files.launch.configurations, []);
  assert.equal(firstLaunchConfiguration(files), undefined);
});

test('requiredExtension names the extension for each type frost emits', () => {
  assert.deepEqual(requiredExtension('cppdbg'), {
    id: 'ms-vscode.cpptools',
    name: 'C/C++',
  });
  assert.deepEqual(requiredExtension('java'), {
    id: 'vscjava.vscode-java-debug',
    name: 'Debugger for Java',
  });
  assert.deepEqual(requiredExtension('debugpy'), {
    id: 'ms-python.debugpy',
    name: 'Python Debugger',
  });
});

test('requiredExtension reports node as needing nothing', () => {
  // Not an oversight and not a missing entry: the JavaScript debugger ships
  // inside VS Code, so there is no marketplace id to show. Any id here would
  // send the user to a page that does not exist, which is why "built in" and
  // "provided by an extension" have to be distinguishable answers.
  assert.equal(requiredExtension('node'), undefined);
});

test('requiredExtension is undefined for an unknown type', () => {
  // A future frost flavor, or a hand-edited launch.json. We do not know what
  // provides it and must not guess an id from the type name.
  assert.equal(requiredExtension('lldb-dap'), undefined);
  assert.equal(requiredExtension(''), undefined);
  // Inherited members are not entries: `in` would answer true for these and
  // report `Object.prototype.constructor` as an extension.
  assert.equal(requiredExtension('constructor'), undefined);
  assert.equal(requiredExtension('toString'), undefined);
});

test('requiredExtension does not hand out its own table', () => {
  // A caller that decorates the result for display would otherwise edit what
  // every later lookup returns.
  const first = requiredExtension('cppdbg');
  assert.notEqual(first, undefined);
  (first as { name: string }).name = 'edited';
  assert.deepEqual(requiredExtension('cppdbg'), {
    id: 'ms-vscode.cpptools',
    name: 'C/C++',
  });
});

test('frostErrorMessage strips the prefix and keeps the guidance verbatim', () => {
  // The whole point: this message names the profile and the exact lines to add
  // to frost.toml. Anything generic in its place ("frost ide failed (1)") loses
  // the only part of the output that helps.
  const text = [
    'frost: error: target "//apps/cli:cli" is not compiled with debug symbols in profile "debug"; add [profile.debug] cflags = ["-O0", "-g"]',
    '',
  ].join('\n');

  assert.equal(
    frostErrorMessage(text),
    'target "//apps/cli:cli" is not compiled with debug symbols in profile "debug"; add [profile.debug] cflags = ["-O0", "-g"]',
  );
});

test('frostErrorMessage keeps colons inside the message', () => {
  // frost prints anyhow chains as `outer: inner`, so splitting on the last
  // colon or on every colon mangles a nested error into its innermost clause.
  assert.equal(
    frostErrorMessage('frost: error: failed to open JAR /w/app.jar: No such file or directory'),
    'failed to open JAR /w/app.jar: No such file or directory',
  );
});

test('frostErrorMessage returns the first of several', () => {
  // The first is the cause; the lines after it are what the cause led to.
  // Reporting the last would tell the user about a consequence.
  const text = [
    'frost: error: a wheel has no direct IDE launch configuration; choose a runnable target',
    'frost: error: IDE configuration was not written',
  ].join('\n');

  assert.equal(
    frostErrorMessage(text),
    'a wheel has no direct IDE launch configuration; choose a runnable target',
  );
});

test('frostErrorMessage tolerates CRLF and finds an error among other output', () => {
  const text = [
    'frost: up to date · 9 of 12 actions · 1 ms',
    '[1/3] CC apps/cli/src/main.c (//apps/cli:cli)',
    'frost: error: IDE artifact .frost/bin/debug/apps_cli_cli was not produced',
    '',
  ].join('\r\n');

  // A trailing `\r` left on the message renders as a stray glyph in the
  // notification, so the assertion is against the exact string.
  assert.equal(
    frostErrorMessage(text),
    'IDE artifact .frost/bin/debug/apps_cli_cli was not produced',
  );
});

test('frostErrorMessage returns undefined when there is no error line', () => {
  // A successful run must not produce a notification, and the summary line
  // shares frost's prefix — `frost: ` alone must not be mistaken for an error.
  assert.equal(frostErrorMessage(CPPDBG_OUTPUT), undefined);
  assert.equal(frostErrorMessage(''), undefined);
  assert.equal(frostErrorMessage('frost: 5 built · 5 actions · 70 ms'), undefined);
  // A wordless error is not guidance either; showing an empty notification is
  // worse than falling back to the caller's generic message.
  assert.equal(frostErrorMessage('frost: error:'), undefined);
  // The word has to be frost's own, not a compiler's.
  assert.equal(frostErrorMessage('core/src/core.c:13:5: error: expected ;'), undefined);
});
