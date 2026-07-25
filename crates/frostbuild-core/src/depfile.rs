use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// How an action reports the inputs it actually read.
///
/// Not every compiler emits a Makefile fragment. MSVC has no `-MF` at all —
/// `/showIncludes` writes its includes to stdout — and a wrapper around a tool
/// with a different dependency protocol can produce a plain list far more
/// easily than valid Makefile escaping.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// `gcc -MD -MF` output, read from the declared depfile path.
    #[default]
    Make,
    /// One path per line, read from the declared depfile path. Blank lines and
    /// `#` comments are ignored, so a shell or Python wrapper can emit it
    /// without escaping rules.
    Lines,
    /// `cl.exe /showIncludes` notes, read from the action's captured stdout
    /// rather than from a file, because that is where MSVC writes them.
    ShowIncludes,
}

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Make => "make",
            Format::Lines => "lines",
            Format::ShowIncludes => "showincludes",
        }
    }

    /// Does this format read the action's captured output instead of a file?
    pub fn reads_captured_output(self) -> bool {
        matches!(self, Format::ShowIncludes)
    }
}

/// Parse dependency paths in the given format.
pub fn parse_format(format: Format, text: &str, workspace_root: &Path) -> Result<Vec<String>> {
    match format {
        Format::Make => parse(text, workspace_root),
        Format::Lines => Ok(relativize(
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(str::to_string)
                .collect(),
            workspace_root,
        )),
        Format::ShowIncludes => Ok(relativize(showincludes_paths(text), workspace_root)),
    }
}

/// The include paths in `/showIncludes` output.
///
/// MSVC writes `Note: including file:` followed by indentation proportional to
/// include depth. The prefix is localized, so matching it literally would work
/// only on an English toolchain; the invariant that does hold on every locale
/// is `<prefix>: <indent><path>` on a line the compiler wrote for one include,
/// so the path is taken after the last `: ` separator.
fn showincludes_paths(text: &str) -> BTreeSet<String> {
    text.lines().filter_map(showincludes_note).collect()
}

/// The path in one `/showIncludes` note, if this line is one.
///
/// The path follows the last `: `, which is what makes this locale-independent:
/// the note prefix itself is translated, the path is not. A diagnostic
/// (`main.c(3): error C2065: 'x': undeclared identifier`) is rejected because
/// it carries a source position before the first separator and its trailing
/// text is prose rather than a path.
fn showincludes_note(line: &str) -> Option<String> {
    let (prefix, path) = line.rsplit_once(": ")?;
    if prefix.contains('(') {
        return None;
    }
    let path = path.trim();
    // MSVC reports every include by full path, so a candidate without a
    // separator is prose from some other message.
    if path.is_empty() || !(path.contains('\\') || path.contains('/')) {
        return None;
    }
    Some(path.replace('\\', "/"))
}

/// Remove the `/showIncludes` notes from captured output, leaving real
/// diagnostics. Without this every rebuild would log the entire include tree.
pub fn strip_showincludes(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    for line in text.lines() {
        if showincludes_note(line).is_none() {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    kept
}

fn relativize(paths: BTreeSet<String>, workspace_root: &Path) -> Vec<String> {
    // Reported paths are normalized to forward slashes, so the root has to be
    // as well or a Windows workspace prefix would never match.
    let root = workspace_root.to_string_lossy().replace('\\', "/");
    let root_prefix = format!("{}/", root.trim_end_matches('/'));
    // Relativizing changes the sort order and can turn two spellings of one
    // path into the same string, so the set is rebuilt afterwards: a recorded
    // dependency list must not depend on which form the tool printed.
    paths
        .into_iter()
        .map(|path| {
            path.strip_prefix(&root_prefix)
                .map(str::to_string)
                .unwrap_or(path)
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Parse a Makefile-style depfile (`gcc -MD -MF` output) and return the
/// dependency paths, deduplicated, with targets excluded.
///
/// Handles: `\`-newline continuations, `\ ` escaped spaces, `\\`, `\#`,
/// `$$`, and multiple rules in one file (gcc emits phony rules for headers).
/// Paths under `workspace_root` are returned workspace-relative; others
/// (system headers) stay absolute.
pub fn parse(text: &str, workspace_root: &Path) -> Result<Vec<String>> {
    let mut deps: BTreeSet<String> = BTreeSet::new();
    let mut targets: BTreeSet<String> = BTreeSet::new();

    let mut token = String::new();
    let mut in_deps = false; // false: reading targets, true: reading deps
    let mut chars = text.chars().peekable();

    let flush = |token: &mut String,
                 in_deps: bool,
                 deps: &mut BTreeSet<String>,
                 targets: &mut BTreeSet<String>| {
        if token.is_empty() {
            return;
        }
        let t = std::mem::take(token);
        if in_deps {
            deps.insert(t);
        } else {
            targets.insert(t);
        }
    };

    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.peek() {
                Some('\n') => {
                    chars.next();
                }
                Some('\r') => {
                    chars.next();
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                }
                Some(' ') => {
                    chars.next();
                    token.push(' ');
                }
                Some('#') => {
                    chars.next();
                    token.push('#');
                }
                Some('\\') => {
                    chars.next();
                    token.push('\\');
                }
                _ => token.push('\\'),
            },
            '$' if chars.peek() == Some(&'$') => {
                chars.next();
                token.push('$');
            }
            ':' if !in_deps => {
                // `foo.o:` — colon terminates the target list. A colon inside
                // a later dep token (rare, e.g. absolute Windows paths) is
                // kept verbatim because in_deps is already true.
                flush(&mut token, false, &mut deps, &mut targets);
                in_deps = true;
            }
            '\n' => {
                flush(&mut token, in_deps, &mut deps, &mut targets);
                in_deps = false; // next rule starts with its target
            }
            c if c.is_whitespace() => {
                flush(&mut token, in_deps, &mut deps, &mut targets);
            }
            c => token.push(c),
        }
    }
    flush(&mut token, in_deps, &mut deps, &mut targets);

    Ok(relativize(
        deps.into_iter()
            .filter(|dep| !targets.contains(dep))
            .collect(),
        workspace_root,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_simple(text: &str) -> Vec<String> {
        parse(text, Path::new("/ws")).unwrap()
    }

    #[test]
    fn parses_basic_rule() {
        let deps = parse_simple("main.o: src/main.c include/util.h\n");
        assert_eq!(deps, vec!["include/util.h", "src/main.c"]);
    }

    #[test]
    fn handles_line_continuations() {
        let deps = parse_simple("main.o: src/main.c \\\n  include/util.h \\\n  include/other.h\n");
        assert_eq!(
            deps,
            vec!["include/other.h", "include/util.h", "src/main.c"]
        );
    }

    #[test]
    fn handles_escaped_spaces() {
        let deps = parse_simple("main.o: src/my\\ file.c\n");
        assert_eq!(deps, vec!["src/my file.c"]);
    }

    #[test]
    fn excludes_phony_header_targets() {
        // gcc -MP emits phony rules like `include/util.h:` after the main rule.
        let deps = parse_simple("main.o: src/main.c include/util.h\n\ninclude/util.h:\n");
        assert_eq!(deps, vec!["src/main.c"]);
    }

    #[test]
    fn relativizes_paths_under_root() {
        let deps = parse(
            "main.o: /ws/src/main.c /usr/include/stdio.h\n",
            Path::new("/ws"),
        )
        .unwrap();
        assert_eq!(deps, vec!["/usr/include/stdio.h", "src/main.c"]);
    }

    #[test]
    fn reads_a_plain_path_list() {
        let deps = parse_format(
            Format::Lines,
            "src/main.ts\n\n# generated by the wrapper\n/ws/src/util.ts\n  src/dup.ts  \nsrc/dup.ts\n",
            Path::new("/ws"),
        )
        .unwrap();
        assert_eq!(deps, vec!["src/dup.ts", "src/main.ts", "src/util.ts"]);
    }

    #[test]
    fn reads_showincludes_notes_without_depending_on_the_locale() {
        // English, German and Japanese cl.exe write a different prefix and the
        // same trailing path; diagnostics and the echoed source name are not
        // includes.
        let captured = "main.c\n\
             Note: including file: C:\\ws\\include\\util.h\n\
             Hinweis: Einlesen der Datei: C:\\ws\\include\\util.h\n\
             \u{30e1}\u{30e2}: \u{5305}\u{542b}\u{30d5}\u{30a1}\u{30a4}\u{30eb}: C:\\sdk\\stdio.h\n\
             main.c(3): error C2065: 'x': undeclared identifier\n\
             cl : Command line warning D9002 : ignoring unknown option\n";
        let deps = parse_format(Format::ShowIncludes, captured, Path::new("C:\\ws")).unwrap();
        assert_eq!(deps, vec!["C:/sdk/stdio.h", "include/util.h"]);

        // The notes leave the log, the diagnostics stay in it.
        let stripped = strip_showincludes(captured);
        assert!(stripped.contains("error C2065"), "{stripped}");
        assert!(stripped.contains("D9002"), "{stripped}");
        assert!(stripped.starts_with("main.c\n"), "{stripped}");
        assert!(!stripped.contains("util.h"), "{stripped}");
    }

    #[test]
    fn handles_dollar_and_hash_escapes() {
        let deps = parse_simple("main.o: src/a$$b.c src/c\\#d.c\n");
        assert_eq!(deps, vec!["src/a$b.c", "src/c#d.c"]);
    }
}
