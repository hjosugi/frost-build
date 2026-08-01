//! Where in a `frost.toml` the cursor is.
//!
//! Deliberately a scanner and not a parser. Completion has to work on a
//! document that does not parse — the moment after `deps = ["` is typed is
//! exactly when the offer is wanted, and that is not valid TOML. So this reads
//! lines, tracks the section header and the open array, and answers three
//! questions: which table, which key, and which string literal.
//!
//! Nothing here decides anything about the build. What a label means, whether
//! a target exists, and what is wrong with a manifest are all the loader's
//! answers; this only says where to put them.

/// A string literal in the document, with the range an editor would replace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Literal {
    pub text: String,
    pub line: usize,
    /// UTF-16 code units, which is what an LSP position counts.
    pub start: usize,
    pub end: usize,
}

/// What the cursor is sitting in.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Cursor {
    /// The `[section]` in effect, without brackets: `target.app`, `workspace`.
    pub section: Option<String>,
    /// The target name, when the section is `target.<name>`.
    pub target: Option<String>,
    /// The key whose value the cursor is in.
    pub key: Option<String>,
    /// The string literal under the cursor.
    pub literal: Option<Literal>,
    /// The cursor is where a key name would be typed: at the start of a line
    /// inside a table, with no `=` before it.
    pub at_key: bool,
}

/// Locate `(line, character)`, where `character` counts UTF-16 code units.
pub fn locate(text: &str, line: usize, character: usize) -> Cursor {
    let lines: Vec<&str> = text.lines().collect();
    let mut cursor = Cursor::default();
    let mut depth = 0usize;
    let mut array_key: Option<String> = None;

    for (index, raw) in lines.iter().enumerate().take(line + 1) {
        let scan = scan(raw);
        if index == line {
            // A header names its own table. Hovering the `[target.app]` line
            // is the most natural way to ask about `app`, and a scan that only
            // looked at earlier lines would answer nothing there.
            if depth == 0 {
                if let Some(header) = section_header(raw, &scan) {
                    set_section(&mut cursor, header);
                }
            }
            let byte = utf16_to_byte(raw, character);
            // An array that closes earlier on this line has closed by the
            // time the cursor is reached, so the depth that matters is the one
            // at the cursor rather than the one the line started with.
            let here = (depth as isize + scan.depth_before(raw, byte)).max(0) as usize;
            cursor.literal = scan.literal_at(raw, index, byte);
            cursor.key = key_before(raw, &scan, byte)
                .map(str::to_string)
                .or_else(|| (here > 0).then(|| array_key.clone()).flatten());
            cursor.at_key = here == 0
                && cursor.literal.is_none()
                && !scan.code(raw)[..byte.min(scan.code_len)].contains('=')
                && !raw.trim_start().starts_with('[');
            break;
        }
        // A header only counts when the line is not continuing an array: a
        // `["//a:b"]` element is not a section, and neither is `[` on its own.
        if depth == 0 {
            if let Some(header) = section_header(raw, &scan) {
                set_section(&mut cursor, header);
            }
        }
        let (opened, key) = scan.array_delta(raw, depth);
        if depth == 0 && opened > 0 {
            array_key = key.map(str::to_string);
        }
        depth = (depth as isize + scan.depth_delta(raw)).max(0) as usize;
        if depth == 0 {
            array_key = None;
        }
    }
    cursor
}

fn set_section(cursor: &mut Cursor, header: &str) {
    cursor.section = Some(header.to_string());
    cursor.target = header
        .strip_prefix("target.")
        .map(|name| name.trim_matches(['"', '\'']).to_string());
}

/// Convert an LSP character offset (UTF-16 code units) to a byte offset.
pub fn utf16_to_byte(line: &str, character: usize) -> usize {
    let mut units = 0usize;
    for (byte, ch) in line.char_indices() {
        if units >= character {
            return byte;
        }
        units += ch.len_utf16();
    }
    line.len()
}

/// The inverse, for reporting a position frost found by byte offset.
pub fn byte_to_utf16(line: &str, byte: usize) -> usize {
    line[..byte.min(line.len())]
        .chars()
        .map(char::len_utf16)
        .sum()
}

/// String literals on one line, and where an unquoted `#` starts a comment.
struct Scan {
    /// `(content start, content end, opening quote)`, in bytes.
    strings: Vec<(usize, usize, char)>,
    code_len: usize,
}

impl Scan {
    fn code<'a>(&self, line: &'a str) -> &'a str {
        &line[..self.code_len]
    }

    fn literal_at(&self, line: &str, index: usize, byte: usize) -> Option<Literal> {
        // Inclusive of both ends, so a cursor just inside either quote counts:
        // that is where a completion is requested from.
        let &(start, end, _) = self
            .strings
            .iter()
            .find(|&&(start, end, _)| byte >= start && byte <= end)?;
        Some(Literal {
            text: line[start..end].to_string(),
            line: index,
            start: byte_to_utf16(line, start),
            end: byte_to_utf16(line, end),
        })
    }

    /// Bracket depth this line adds, ignoring brackets inside strings and a
    /// section header.
    fn depth_delta(&self, line: &str) -> isize {
        self.depth_before(line, line.len())
    }

    /// The same, counting only the brackets before `byte`.
    fn depth_before(&self, line: &str, byte: usize) -> isize {
        if section_header(line, self).is_some() {
            return 0;
        }
        self.outside_strings(line)
            .take_while(|&(at, _)| at < byte)
            .map(|(_, ch)| match ch {
                '[' => 1,
                ']' => -1,
                _ => 0,
            })
            .sum()
    }

    /// Whether this line opens an array, and under which key.
    fn array_delta<'a>(&self, line: &'a str, depth: usize) -> (usize, Option<&'a str>) {
        if depth > 0 || section_header(line, self).is_some() {
            return (0, None);
        }
        let opened = self
            .outside_strings(line)
            .filter(|&(_, ch)| ch == '[')
            .count();
        (opened, key_of(self.code(line)))
    }

    fn outside_strings<'a>(&'a self, line: &'a str) -> impl Iterator<Item = (usize, char)> + 'a {
        line[..self.code_len]
            .char_indices()
            .filter(move |&(at, _)| {
                !self
                    .strings
                    .iter()
                    .any(|&(start, end, _)| at >= start && at < end)
            })
    }
}

fn scan(line: &str) -> Scan {
    let mut strings = Vec::new();
    let mut code_len = line.len();
    let bytes = line.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] as char {
            quote @ ('"' | '\'') => {
                let content = index + 1;
                let mut end = content;
                while end < bytes.len() {
                    // Escapes exist in basic strings only; a literal string
                    // ends at its first quote, backslashes and all.
                    if quote == '"' && bytes[end] == b'\\' {
                        end += 2;
                        continue;
                    }
                    if bytes[end] as char == quote {
                        break;
                    }
                    end += 1;
                }
                let end = end.min(bytes.len());
                strings.push((content, end, quote));
                // An unterminated string — which is what half-typed input is —
                // runs to the end of the line, and so does the code.
                index = if end < bytes.len() { end + 1 } else { end };
            }
            '#' => {
                code_len = index;
                break;
            }
            _ => index += 1,
        }
    }
    Scan { strings, code_len }
}

fn section_header<'a>(line: &'a str, scan: &Scan) -> Option<&'a str> {
    let code = scan.code(line).trim();
    // `[[array.of.tables]]` is not part of this manifest grammar; a single
    // pair is the only header shape.
    code.strip_prefix('[')?
        .strip_suffix(']')
        .filter(|inner| !inner.starts_with('['))
}

/// The key a `key = value` line assigns to.
fn key_of(code: &str) -> Option<&str> {
    let (key, _) = code.split_once('=')?;
    let key = key.trim();
    (!key.is_empty() && !key.contains(['[', ']'])).then_some(key)
}

/// The key whose value contains `byte`, when the assignment is on this line.
fn key_before<'a>(line: &'a str, scan: &Scan, byte: usize) -> Option<&'a str> {
    let code = scan.code(line);
    let equals = code.find('=')?;
    (byte > equals).then(|| key_of(code))?
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = "\
[workspace]
default_targets = [\"//apps/cli:cli\"]

# A comment with a [bracket] and a \"quote\".
[target.app]
kind = \"cc_binary\"
srcs = [\"src/main.c\"]
deps = [
  \"//core:core\",
  \"//text:text\",
]
";

    fn at(line: usize, character: usize) -> Cursor {
        locate(MANIFEST, line, character)
    }

    #[test]
    fn a_section_header_names_the_table_the_cursor_is_in() {
        assert_eq!(
            at(0, 2).section.as_deref(),
            Some("workspace"),
            "a header names its own table, which is where a hover asks from"
        );
        assert_eq!(at(4, 4).target.as_deref(), Some("app"), "on [target.app]");
        assert_eq!(at(1, 0).section.as_deref(), Some("workspace"));
        assert_eq!(at(5, 0).section.as_deref(), Some("target.app"));
        assert_eq!(at(5, 0).target.as_deref(), Some("app"));
        assert_eq!(at(8, 4).target.as_deref(), Some("app"));
    }

    #[test]
    fn a_comment_is_not_a_section_and_its_brackets_are_not_arrays() {
        // Line 3 is a comment containing a bracket pair and a quote. If it
        // were scanned as code, every position after it would be wrong.
        assert_eq!(at(5, 0).section.as_deref(), Some("target.app"));
        assert_eq!(at(5, 8).key.as_deref(), Some("kind"));
    }

    #[test]
    fn the_key_under_the_cursor_is_the_one_being_assigned() {
        assert_eq!(at(5, 9).key.as_deref(), Some("kind"));
        assert_eq!(at(6, 10).key.as_deref(), Some("srcs"));
        // Before the `=` is a key position, not a value position.
        assert_eq!(at(5, 2).key, None);
        assert!(
            at(5, 2).at_key,
            "a half-typed key is where keys are offered"
        );
    }

    #[test]
    fn a_multi_line_array_keeps_naming_its_key() {
        // This is the case that makes a scanner necessary: the cursor is three
        // lines below the `deps =` that gives it meaning.
        assert_eq!(at(8, 5).key.as_deref(), Some("deps"));
        assert_eq!(at(9, 5).key.as_deref(), Some("deps"));
        assert_eq!(
            at(8, 5).literal.as_ref().map(|l| l.text.as_str()),
            Some("//core:core")
        );
        // And it stops naming it after the array closes.
        assert_eq!(at(10, 1).key, None);
    }

    #[test]
    fn a_literal_is_found_from_either_of_its_quotes_inward() {
        let inside = at(5, 9).literal.expect("inside the value");
        assert_eq!(inside.text, "cc_binary");
        // Just after the opening quote is where a client asks for completion
        // the instant the quote is typed.
        assert_eq!(at(5, 8).literal.map(|l| l.text), Some("cc_binary".into()));
        assert_eq!(inside.start, 8);
        assert_eq!(inside.end, 17);
        assert_eq!(at(5, 0).literal, None, "the key is not a literal");
    }

    #[test]
    fn an_unterminated_string_still_locates() {
        // What a document looks like the moment `"` is typed, which is exactly
        // when completion is requested.
        let typing = "[target.app]\nkind = \"";
        let cursor = locate(typing, 1, 8);
        assert_eq!(cursor.target.as_deref(), Some("app"));
        assert_eq!(cursor.key.as_deref(), Some("kind"));
        assert_eq!(cursor.literal.map(|l| l.text), Some(String::new()));

        let typing = "[target.app]\ndeps = [\n  \"//co";
        let cursor = locate(typing, 2, 7);
        assert_eq!(cursor.key.as_deref(), Some("deps"));
        assert_eq!(cursor.literal.map(|l| l.text), Some("//co".into()));
    }

    #[test]
    fn positions_count_utf16_units_the_way_an_editor_does() {
        // A comment with an astral character before the value: a byte offset
        // and an LSP character offset disagree from there on.
        let line = "cmd = \"🧊 ${in}\"";
        assert_eq!(utf16_to_byte(line, 7), 6 + 1);
        assert_eq!(
            byte_to_utf16(line, line.len()),
            line.chars().map(char::len_utf16).sum::<usize>()
        );

        let text = format!("[target.a]\n{line}\n");
        let cursor = locate(&text, 1, 9);
        assert_eq!(cursor.key.as_deref(), Some("cmd"));
        assert_eq!(cursor.literal.map(|l| l.text), Some("🧊 ${in}".into()));
    }
}
