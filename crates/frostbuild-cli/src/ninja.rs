//! `frost import-ninja`: turn a `build.ninja` into a Frost manifest.
//!
//! This is a migration aid for a small, explicit Ninja subset — the one
//! `docs/06_ninja_importer.md` describes — not Ninja compatibility. Every
//! edge becomes a `genrule` that runs the edge's command through the host
//! shell, with the edge's outputs declared and its inputs split into files
//! and producing targets.
//!
//! The rule it follows is the one every Frost importer follows: what cannot be
//! translated exactly is refused, with the line and the reason, rather than
//! dropped. A `depfile` the genrule cannot read would leave header changes
//! unnoticed; a per-edge binding ignored would run a different command; a path
//! outside the directory would name a file the manifest cannot own. Each of
//! those produces a build that works today and is silently stale tomorrow,
//! which is worse than an importer that says no.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

pub(crate) fn import_ninja(root: &Path, ninja: PathBuf, output: PathBuf) -> Result<i32> {
    let source = if ninja.is_absolute() {
        ninja
    } else {
        root.join(ninja)
    };
    let destination = if output.is_absolute() {
        output
    } else {
        root.join(output)
    };
    // Paths inside build.ninja are relative to the directory Ninja runs in,
    // which is the directory holding the file; a manifest's paths are
    // relative to the manifest. Writing the manifest anywhere else would
    // silently re-root every path in it.
    let directory_of = |path: &Path| {
        let parent = path.parent().unwrap_or(Path::new("."));
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        std::fs::canonicalize(parent).with_context(|| format!("resolving {}", parent.display()))
    };
    if directory_of(&source)? != directory_of(&destination)? {
        bail!(
            "write the manifest next to {}: its paths are relative to that directory, \
             and a manifest elsewhere would re-root every one of them",
            source.display()
        );
    }
    if destination.exists() {
        bail!(
            "{} already exists; frost import-ninja never overwrites a manifest. \
             Remove it, or pass --output with a new file name to compare",
            destination.display()
        );
    }
    let text = std::fs::read_to_string(&source)
        .with_context(|| format!("reading {}", source.display()))?;
    let name = source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "build.ninja".to_string());
    let manifest = convert(&text, &name)?;
    std::fs::write(&destination, &manifest)
        .with_context(|| format!("writing {}", destination.display()))?;
    println!("frost: imported {}", source.display());
    println!("  wrote {}", destination.display());
    println!("  review it, then: frost build");
    Ok(0)
}

/// One `build` statement, with paths already evaluated.
struct Edge {
    line: usize,
    rule: String,
    outputs: Vec<String>,
    implicit_outputs: Vec<String>,
    inputs: Vec<String>,
    implicit_inputs: Vec<String>,
    order_only: Vec<String>,
}

/// What a `rule` block may say. Everything else is refused by name.
#[derive(Default)]
struct Rule {
    command: Option<String>,
}

enum Block {
    None,
    Rule(String),
    Build,
}

fn convert(text: &str, file_name: &str) -> Result<String> {
    let mut variables: BTreeMap<String, String> = BTreeMap::new();
    let mut rules: BTreeMap<String, Rule> = BTreeMap::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut defaults: Vec<(usize, String)> = Vec::new();
    let mut block = Block::None;

    for (line, text) in logical_lines(text) {
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let at = |message: String| anyhow::anyhow!("{file_name}:{line}: {message}");
        if text.starts_with([' ', '\t']) {
            let (key, value) = trimmed
                .split_once('=')
                .map(|(key, value)| (key.trim(), value.trim_start()))
                .ok_or_else(|| at(format!("expected `name = value`, found {trimmed:?}")))?;
            match &block {
                Block::Rule(name) => match key {
                    "command" => {
                        rules.get_mut(name).expect("rule block").command = Some(value.to_string())
                    }
                    // Only shown by Ninja while the command runs.
                    "description" => {}
                    "depfile" | "deps" | "msvc_deps_prefix" => {
                        return Err(at(format!(
                            "rule {name:?} sets `{key}`: the headers it reports would not reach \
                             a genrule's inputs, so a header edit would not rebuild. Translate \
                             compile edges to cc_library/cc_binary targets (their dependency \
                             tracking is built in), or remove `{key}` and list every header in \
                             the edge's inputs"
                        )))
                    }
                    other => {
                        return Err(at(format!(
                            "rule {name:?} sets `{other}`, which frost import-ninja does not \
                             translate (supported: command, description)"
                        )))
                    }
                },
                Block::Build => {
                    return Err(at(format!(
                        "per-edge binding `{key} = ...` is not supported: the edge would run a \
                         different command than the rule says. Inline it into a rule of its own"
                    )))
                }
                Block::None => return Err(at(format!("unexpected indented line {trimmed:?}"))),
            }
            continue;
        }
        block = Block::None;
        let (keyword, rest) = trimmed
            .split_once(char::is_whitespace)
            .map(|(keyword, rest)| (keyword, rest.trim()))
            .unwrap_or((trimmed, ""));
        match keyword {
            "rule" => {
                if rest.is_empty() || rules.insert(rest.to_string(), Rule::default()).is_some() {
                    return Err(at(format!("rule {rest:?} is empty or defined twice")));
                }
                block = Block::Rule(rest.to_string());
            }
            "build" => {
                edges.push(parse_edge(rest, &variables).map_err(|error| at(error.to_string()))?);
                edges.last_mut().expect("just pushed").line = line;
                block = Block::Build;
            }
            "default" => {
                for path in split_paths(rest, &variables).map_err(|error| at(error.to_string()))? {
                    defaults.push((line, path));
                }
            }
            "include" | "subninja" | "pool" => {
                return Err(at(format!(
                    "`{keyword}` is not supported; import a build.ninja that is one \
                     self-contained file without pools"
                )))
            }
            _ => {
                let Some((name, value)) = trimmed.split_once('=') else {
                    return Err(at(format!("unsupported Ninja syntax {trimmed:?}")));
                };
                let name = name.trim();
                if name.is_empty() || !name.chars().all(is_variable_char) {
                    return Err(at(format!("unsupported Ninja syntax {trimmed:?}")));
                }
                // Top-level variables are evaluated when they are defined.
                let value = evaluate(value.trim_start(), &|key| Ok(variables.get(key).cloned()))
                    .map_err(|error| at(error.to_string()))?;
                variables.insert(name.to_string(), value);
            }
        }
    }

    render(file_name, &variables, &rules, &edges, &defaults)
}

fn render(
    file_name: &str,
    variables: &BTreeMap<String, String>,
    rules: &BTreeMap<String, Rule>,
    edges: &[Edge],
    defaults: &[(usize, String)],
) -> Result<String> {
    // `build ALIAS: phony PATHS` names a set of paths and builds nothing.
    let mut aliases: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in edges.iter().filter(|edge| edge.rule == "phony") {
        let members: Vec<&str> = edge
            .inputs
            .iter()
            .chain(&edge.implicit_inputs)
            .chain(&edge.order_only)
            .map(String::as_str)
            .collect();
        for output in edge.outputs.iter().chain(&edge.implicit_outputs) {
            aliases.insert(output, members.clone());
        }
    }
    let real: Vec<&Edge> = edges.iter().filter(|edge| edge.rule != "phony").collect();

    let mut producer: BTreeMap<&str, String> = BTreeMap::new();
    let mut names = BTreeSet::new();
    for edge in &real {
        let name = sanitize_target(&edge.outputs[0]);
        if !names.insert(name.clone()) {
            bail!(
                "{file_name}:{}: target name {name:?} (from {:?}) collides with another edge's",
                edge.line,
                edge.outputs[0]
            );
        }
        for output in edge.outputs.iter().chain(&edge.implicit_outputs) {
            if aliases.contains_key(output.as_str()) {
                bail!(
                    "{file_name}:{}: {output:?} is both a phony alias and an output",
                    edge.line
                );
            }
            if producer.insert(output, name.clone()).is_some() {
                bail!(
                    "{file_name}:{}: {output:?} is produced by two edges",
                    edge.line
                );
            }
        }
    }

    // Resolve a path through phony aliases to the files it stands for.
    fn resolve<'a>(
        path: &'a str,
        aliases: &BTreeMap<&'a str, Vec<&'a str>>,
        seen: &mut BTreeSet<&'a str>,
        out: &mut Vec<&'a str>,
    ) {
        match aliases.get(path) {
            Some(members) => {
                if seen.insert(path) {
                    for member in members {
                        resolve(member, aliases, seen, out);
                    }
                }
            }
            None => out.push(path),
        }
    }

    let mut manifest = format!(
        "# Generated by `frost import-ninja` from {file_name}.\n\
         # Each Ninja edge became a genrule that runs its command through the host\n\
         # shell in this directory. Review it before trusting it; see\n\
         # docs/guide/migrate/ninja.md for what to translate next.\n\n\
         [workspace]\n"
    );
    if !defaults.is_empty() {
        let mut targets = Vec::new();
        for (line, path) in defaults {
            let mut files = Vec::new();
            resolve(path, &aliases, &mut BTreeSet::new(), &mut files);
            for file in files {
                let Some(name) = producer.get(file) else {
                    bail!("{file_name}:{line}: default {file:?} is not produced by any edge");
                };
                if !targets.contains(name) {
                    targets.push(name.clone());
                }
            }
        }
        manifest.push_str(&format!("default_targets = {}\n", toml_array(&targets)?));
    }
    manifest.push('\n');

    for edge in real {
        let rule = rules.get(&edge.rule).with_context(|| {
            format!(
                "{file_name}:{}: unknown Ninja rule {:?}",
                edge.line, edge.rule
            )
        })?;
        let command = rule.command.as_deref().with_context(|| {
            format!(
                "{file_name}:{}: rule {:?} has no command",
                edge.line, edge.rule
            )
        })?;
        let joined = |paths: &[String]| {
            paths
                .iter()
                .map(|path| shell_escape(path))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let cmd = evaluate(command, &|key| match key {
            "in" => Ok(Some(joined(&edge.inputs))),
            "out" => Ok(Some(joined(&edge.outputs))),
            "in_newline" => bail!("`$in_newline` is not supported"),
            other => Ok(variables.get(other).cloned()),
        })
        .with_context(|| format!("{file_name}:{}: rule {:?}", edge.line, edge.rule))?;
        if cmd.contains("${") {
            bail!(
                "{file_name}:{}: the command contains `${{`, which a Frost genrule would read as \
                 its own substitution",
                edge.line
            );
        }

        let name = &producer[edge.outputs[0].as_str()];
        let mut inputs: Vec<String> = Vec::new();
        let mut deps: Vec<String> = Vec::new();
        for path in edge
            .inputs
            .iter()
            .chain(&edge.implicit_inputs)
            .chain(&edge.order_only)
        {
            let mut files = Vec::new();
            resolve(path, &aliases, &mut BTreeSet::new(), &mut files);
            for file in files {
                match producer.get(file) {
                    Some(dep) if dep != name => {
                        if !deps.contains(dep) {
                            deps.push(dep.clone());
                        }
                    }
                    Some(_) => {}
                    None => {
                        if !inputs.iter().any(|input| input == file) {
                            inputs.push(file.to_string());
                        }
                    }
                }
            }
        }
        let outputs: Vec<String> = edge
            .outputs
            .iter()
            .chain(&edge.implicit_outputs)
            .cloned()
            .collect();
        manifest.push_str(&format!(
            "[target.{name}]\nkind = \"genrule\"\ncmd = {}\ninputs = {}\noutputs = {}\ndeps = {}\n\n",
            serde_json::to_string(&cmd)?,
            toml_array(&inputs)?,
            toml_array(&outputs)?,
            toml_array(&deps)?,
        ));
    }
    Ok(manifest)
}

/// `build OUT [| IMPLICIT_OUT]: RULE IN [| IMPLICIT] [|| ORDER_ONLY]`.
fn parse_edge(rest: &str, variables: &BTreeMap<String, String>) -> Result<Edge> {
    let colon = unescaped_colon(rest).context("a build statement needs `outputs: rule inputs`")?;
    let (outputs, implicit_outputs) =
        match split_groups(&rest[..colon], variables, &["|"])?.as_slice() {
            [explicit] => (explicit.clone(), Vec::new()),
            [explicit, implicit] => (explicit.clone(), implicit.clone()),
            _ => bail!("more than one `|` among the outputs"),
        };
    if outputs.is_empty() {
        bail!("a build statement needs at least one explicit output");
    }
    let right = rest[colon + 1..].trim_start();
    let (rule, inputs) = right
        .split_once(char::is_whitespace)
        .map(|(rule, inputs)| (rule, inputs.trim()))
        .unwrap_or((right, ""));
    if rule.is_empty() {
        bail!("a build statement needs a rule");
    }
    if inputs.contains("|@") {
        bail!("validations (`|@`) are not supported");
    }
    let groups = split_groups(inputs, variables, &["|", "||"])?;
    let mut groups = groups.into_iter();
    let explicit = groups.next().unwrap_or_default();
    let mut implicit = Vec::new();
    let mut order_only = Vec::new();
    // split_groups keeps the separators' order, so re-scan the raw text to
    // know which group followed which separator.
    let separators: Vec<&str> = inputs
        .split_whitespace()
        .filter(|token| *token == "|" || *token == "||")
        .collect();
    for (separator, group) in separators.iter().zip(groups) {
        match *separator {
            "|" if implicit.is_empty() && order_only.is_empty() => implicit = group,
            "||" if order_only.is_empty() => order_only = group,
            _ => bail!("inputs must be `explicit [| implicit] [|| order-only]`"),
        }
    }
    Ok(Edge {
        line: 0,
        rule: rule.to_string(),
        outputs,
        implicit_outputs,
        inputs: explicit,
        implicit_inputs: implicit,
        order_only,
    })
}

/// Split evaluated paths into groups at the given bare separator tokens.
fn split_groups(
    text: &str,
    variables: &BTreeMap<String, String>,
    separators: &[&str],
) -> Result<Vec<Vec<String>>> {
    let mut groups = vec![Vec::new()];
    for token in raw_tokens(text) {
        if separators.contains(&token.as_str()) {
            groups.push(Vec::new());
            continue;
        }
        let path = evaluate(&token, &|key| Ok(variables.get(key).cloned()))?;
        groups
            .last_mut()
            .expect("at least one group")
            .push(checked_path(&path)?);
    }
    Ok(groups)
}

fn split_paths(text: &str, variables: &BTreeMap<String, String>) -> Result<Vec<String>> {
    Ok(split_groups(text, variables, &[])?.concat())
}

/// A path Frost can own: relative, inside this directory, not a glob.
fn checked_path(path: &str) -> Result<String> {
    let path = path.strip_prefix("./").unwrap_or(path);
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.split('/').any(|part| part.is_empty() || part == "..")
        || Path::new(path).is_absolute()
    {
        bail!(
            "path {path:?} is not inside the directory holding build.ninja; Frost paths are \
             relative to the manifest, so an out-of-source build directory cannot be imported \
             as it is"
        );
    }
    if path.contains(['*', '?', '[']) {
        bail!("path {path:?} contains a glob character, which a Frost manifest would expand");
    }
    Ok(path.to_string())
}

/// Whitespace-separated tokens, honouring `$ `, `$:` and `$$` escapes (kept
/// escaped for [`evaluate`]).
fn raw_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '$' => {
                current.push('$');
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn unescaped_colon(text: &str) -> Option<usize> {
    let mut escaped = false;
    for (index, c) in text.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '$' => escaped = true,
            ':' => return Some(index),
            _ => {}
        }
    }
    None
}

/// Evaluate a Ninja string: `$$`, `$ `, `$:` escapes and `$name` / `${name}`
/// references. An unknown variable is empty, as it is in Ninja.
fn evaluate(text: &str, lookup: &dyn Fn(&str) -> Result<Option<String>>) -> Result<String> {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('$') => out.push('$'),
            Some(' ') => out.push(' '),
            Some(':') => out.push(':'),
            Some('{') => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => bail!("unterminated `${{` in {text:?}"),
                    }
                }
                out.push_str(&lookup(&name)?.unwrap_or_default());
            }
            Some(c) if is_variable_char(c) => {
                let mut name = String::from(c);
                while let Some(&next) = chars.peek() {
                    if !is_variable_char(next) {
                        break;
                    }
                    name.push(next);
                    chars.next();
                }
                out.push_str(&lookup(&name)?.unwrap_or_default());
            }
            Some(other) => bail!("invalid escape `${other}` in {text:?}"),
            None => bail!("trailing `$` in {text:?}"),
        }
    }
    Ok(out)
}

fn is_variable_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Physical lines joined across `$`-newline continuations, numbered by the
/// line each logical line starts on.
fn logical_lines(text: &str) -> Vec<(usize, String)> {
    let mut lines = Vec::new();
    let mut pending: Option<(usize, String)> = None;
    for (index, physical) in text.lines().enumerate() {
        let physical = physical.strip_suffix('\r').unwrap_or(physical);
        let (start, mut current) = match pending.take() {
            Some((start, mut current)) => {
                current.push_str(physical.trim_start());
                (start, current)
            }
            None => (index + 1, physical.to_string()),
        };
        // An odd number of trailing `$` is a continuation; `$$` is a dollar.
        let dollars = current.chars().rev().take_while(|c| *c == '$').count();
        if dollars % 2 == 1 {
            current.pop();
            pending = Some((start, current));
        } else {
            lines.push((start, current));
        }
    }
    if let Some(line) = pending {
        lines.push(line);
    }
    lines
}

/// Quote a path the way Ninja does for `$in`/`$out` on POSIX shells.
fn shell_escape(path: &str) -> String {
    if path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_+-./@%,=:".contains(c))
    {
        path.to_string()
    } else {
        format!("'{}'", path.replace('\'', "'\\''"))
    }
}

fn toml_array(values: &[String]) -> Result<String> {
    Ok(serde_json::to_string(values)?)
}

fn sanitize_target(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NINJA: &str = "\
cflags = -O2 -Wall
includes = -Iinclude

rule cc
  command = cc $cflags $includes -c $in -o $out
  description = CC $out

rule link
  command = cc $in -o $out

rule gen
  command = printf '#define ANSWER 42\\n' > $out

build gen/answer.h: gen tools/answer.txt
build out/main.o: cc src/main.c | include/calc.h || gen/answer.h
build out/calc.o: cc src/$
    calc.c
build out/calc: link out/main.o out/calc.o

build all: phony out/calc
default all
";

    #[test]
    fn the_documented_subset_becomes_genrules() {
        let manifest = convert(NINJA, "build.ninja").unwrap();
        // Variables are evaluated, `$in`/`$out` become the edge's own paths in
        // Ninja's order, and a continuation line is part of its statement.
        assert!(
            manifest.contains("cmd = \"cc -O2 -Wall -Iinclude -c src/main.c -o out/main.o\""),
            "{manifest}"
        );
        assert!(
            manifest.contains("cmd = \"cc -O2 -Wall -Iinclude -c src/calc.c -o out/calc.o\""),
            "{manifest}"
        );
        // Produced inputs are dependencies; the link still names them in
        // order, which `${in}` over declared files alone could not.
        assert!(
            manifest.contains(
                "[target.out_calc]\nkind = \"genrule\"\ncmd = \"cc out/main.o out/calc.o -o out/calc\"\n\
                 inputs = []\noutputs = [\"out/calc\"]\ndeps = [\"out_main_o\",\"out_calc_o\"]"
            ),
            "{manifest}"
        );
        // Implicit inputs are files; order-only generated inputs are deps.
        assert!(
            manifest.contains(
                "inputs = [\"src/main.c\",\"include/calc.h\"]\noutputs = [\"out/main.o\"]\n\
                 deps = [\"gen_answer_h\"]"
            ),
            "{manifest}"
        );
        // `default all` resolves through the phony alias; phony is no target.
        assert!(
            manifest.contains("default_targets = [\"out_calc\"]"),
            "{manifest}"
        );
        assert!(!manifest.contains("[target.all]"), "{manifest}");
        // `$$` is a literal dollar, and the command reaches TOML escaped.
        assert!(
            manifest.contains("printf '#define ANSWER 42\\\\n' > gen/answer.h"),
            "{manifest}"
        );

        frostbuild_core::manifest::Manifest::parse_str(&manifest).unwrap();
    }

    #[test]
    fn what_cannot_be_translated_exactly_is_refused_with_its_line() {
        let cases = [
            (
                "rule cc\n  command = cc -c $in\n  depfile = $out.d\n",
                "build.ninja:3:",
                "depfile",
            ),
            (
                "rule cc\n  command = cc -c $in\n  deps = gcc\n",
                "build.ninja:3:",
                "deps",
            ),
            (
                "rule cc\n  command = cc\n  pool = console\n",
                "build.ninja:3:",
                "pool",
            ),
            (
                "rule cc\n  command = cc $flags\nbuild a.o: cc a.c\n  flags = -O2\n",
                "build.ninja:4:",
                "per-edge binding",
            ),
            ("include rules.ninja\n", "build.ninja:1:", "include"),
            ("subninja sub.ninja\n", "build.ninja:1:", "subninja"),
            ("pool link\n  depth = 1\n", "build.ninja:1:", "pool"),
            (
                "rule cc\n  command = cc\nbuild a.o: cc ../src/a.c\n",
                "build.ninja:3:",
                "out-of-source",
            ),
            (
                "rule cc\n  command = cc\nbuild /tmp/a.o: cc a.c\n",
                "build.ninja:3:",
                "out-of-source",
            ),
            (
                "rule cc\n  command = cc\nbuild a.o: cc a.c |@ check\n",
                "build.ninja:3:",
                "validations",
            ),
            (
                "build a.o: missing a.c\n",
                "build.ninja:1:",
                "unknown Ninja rule",
            ),
            (
                "rule cc\n  command = cc\nbuild a.o: cc a.c\nbuild a.o: cc b.c\n",
                "build.ninja:4:",
                "",
            ),
            (
                "rule cc\n  command = cc $in_newline\nbuild a: cc b\n",
                "build.ninja:3:",
                "in_newline",
            ),
            (
                "rule cc\n  command = cc\nbuild a: cc b\ndefault b\n",
                "build.ninja:4:",
                "not produced",
            ),
        ];
        for (text, location, reason) in cases {
            let error = format!("{:#}", convert(text, "build.ninja").unwrap_err());
            assert!(
                error.contains(location) && error.contains(reason),
                "{text:?} -> {error}"
            );
        }
    }

    #[test]
    fn paths_with_shell_metacharacters_are_quoted_like_ninja_quotes_them() {
        let manifest = convert(
            "rule cp\n  command = cp $in $out\nbuild out/a$ b.txt: cp it's.txt\n",
            "build.ninja",
        )
        .unwrap();
        assert!(
            manifest.contains(r#"cmd = "cp 'it'\\''s.txt' 'out/a b.txt'""#),
            "{manifest}"
        );
        assert!(
            manifest.contains(r#"outputs = ["out/a b.txt"]"#),
            "{manifest}"
        );
    }

    #[test]
    fn no_default_statement_leaves_the_frost_default() {
        let manifest = convert("rule t\n  command = touch $out\nbuild a: t\n", "b.ninja").unwrap();
        assert!(!manifest.contains("default_targets"), "{manifest}");
        let parsed = frostbuild_core::manifest::Manifest::parse_str(&manifest).unwrap();
        assert_eq!(parsed.default_targets, ["a"]);
    }
}
