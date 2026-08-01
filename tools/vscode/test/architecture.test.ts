import * as assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';

// The rule the whole test suite depends on: everything under `src/frost/`
// except `cli.ts` is pure, so it can be exercised under plain `node --test`
// with no VS Code download and no display server. That is what makes CI able
// to afford running these on every push.
//
// It is an easy rule to break by accident — one `import * as vscode` inside a
// parser, for a convenience type, and every test in this directory stops being
// runnable. So it is enforced rather than documented.

// Compiled tests run from `out/test/`, so the sources are two levels up. The
// TypeScript is what gets inspected, not the compiled output: a type-only
// import of `vscode` is erased by the compiler and would pass an inspection of
// `out/` while still coupling the module to the editor API.
const FROST_DIR = join(__dirname, '..', '..', 'src', 'frost');

function sourceFiles(): string[] {
  return readdirSync(FROST_DIR)
    .filter((name) => name.endsWith('.ts'))
    .map((name) => join(FROST_DIR, name));
}

test('the frost directory resolves and is populated', () => {
  // Guards the two tests below. If the path stopped resolving they would pass
  // vacuously, which is the worst outcome an architecture test can have — it
  // would report a rule as held while checking nothing.
  const files = sourceFiles();
  assert.ok(
    files.length >= 5,
    `expected the pure modules at ${FROST_DIR}, found ${files.length} files`,
  );
});

test('pure modules never import vscode', () => {
  const offenders = sourceFiles().filter((file) => {
    const text = readFileSync(file, 'utf8');
    return (
      /from\s+['"]vscode['"]/.test(text) || /require\(['"]vscode['"]\)/.test(text)
    );
  });
  assert.deepEqual(
    offenders,
    [],
    'modules under src/frost/ must stay runnable without VS Code',
  );
});

test('only cli.ts reaches outside the process', () => {
  // Spawning is confined to one module so the rest can be tested by handing
  // them captured text. A parser that shells out is a parser that needs a
  // built frost binary — and a workspace — before it can be tested at all.
  const offenders = sourceFiles()
    .filter((file) => !file.endsWith('cli.ts'))
    .filter((file) => {
      const text = readFileSync(file, 'utf8');
      return /node:child_process|node:fs\b/.test(text);
    });
  assert.deepEqual(offenders, [], 'only cli.ts may spawn or read the filesystem');
});
