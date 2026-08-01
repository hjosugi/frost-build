// Path classification, kept pure so the rule can be tested without an editor.
//
// It exists because a diagnostic's path is not always workspace-relative: a
// compiler reporting a system header gives an absolute path, and resolving that
// against the workspace root produces a file that does not exist. The editor
// layer has to branch, so the branch condition lives somewhere it can be
// pinned by a test.

/**
 * Is this an absolute path on any host frost supports?
 *
 * Deliberately not `node:path`'s `isAbsolute`: that answers for the platform
 * the extension happens to run on, and the answer needed here is about the path
 * the compiler produced. A Windows path seen while reading a log on Linux is
 * still absolute.
 */
export function isAbsolutePath(path: string): boolean {
  if (path.startsWith('/')) {
    return true;
  }
  // UNC, in either slash spelling.
  if (path.startsWith('\\\\') || path.startsWith('//')) {
    return true;
  }
  // Drive-qualified: `C:/x`, `C:\x`. A bare `C:` with no separator is a
  // drive-relative path, which is not absolute.
  return /^[A-Za-z]:[\\/]/.test(path);
}
