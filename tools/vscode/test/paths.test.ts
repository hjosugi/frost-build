import * as assert from 'node:assert/strict';
import { test } from 'node:test';

import { isAbsolutePath } from '../src/frost/paths';

test('workspace-relative paths are not absolute', () => {
  // The common case: frost reports these, and they must be resolved against
  // the workspace root.
  for (const path of ['core/src/core.c', 'a.c', './a.c', '../sibling/a.c']) {
    assert.equal(isAbsolutePath(path), false, path);
  }
});

test('a system header is recognised as absolute', () => {
  // The bug this module exists for: joining `/usr/include/stdio.h` onto the
  // workspace root yields a path that does not exist, and the diagnostic is
  // silently attached to nothing.
  assert.equal(isAbsolutePath('/usr/include/stdio.h'), true);
});

test('Windows paths are absolute even when read on another host', () => {
  // A log captured on Windows can be parsed anywhere, so the answer must be
  // about the path rather than about the running platform.
  for (const path of ['C:/msys64/include/stdio.h', 'C:\\msys64\\include\\stdio.h']) {
    assert.equal(isAbsolutePath(path), true, path);
  }
  for (const path of ['\\\\server\\share\\a.c', '//server/share/a.c']) {
    assert.equal(isAbsolutePath(path), true, path);
  }
});

test('a drive-relative path is not absolute', () => {
  // `C:a.c` means "a.c on drive C's current directory" — resolving it against
  // the workspace is as wrong as treating it as absolute, but it is not the
  // case this predicate is claiming to handle, and saying true would send it
  // down the wrong branch.
  assert.equal(isAbsolutePath('C:a.c'), false);
});

test('an empty path is not absolute', () => {
  assert.equal(isAbsolutePath(''), false);
});
