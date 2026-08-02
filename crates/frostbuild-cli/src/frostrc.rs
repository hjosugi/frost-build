//! `.frostrc`: how to build, kept out of `frost.toml`, which says what to build.
//!
//! The mechanism is deliberately small. Rather than parse a config file into a
//! settings struct and merge it with parsed arguments field by field — which
//! means reimplementing clap's type checking and knowing which fields the user
//! actually typed — this turns the file into argument strings and splices them
//! in *ahead* of the real command line. Clap's own last-occurrence-wins rule
//! then produces the documented precedence, and a value from a file is parsed,
//! validated and rejected by exactly the code that handles a typed one.
//!
//! The consequence worth stating: a flag from a file is treated as if it had
//! been typed, including by the action key. Whether a flag is key material is a
//! property of the flag, never of where its value came from.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

pub const WORKSPACE_FILE: &str = ".frostrc";

/// Sections that apply to every subcommand, in the order they are merged.
const COMMON: &str = "common";

/// Where a value came from, so `doctor` can say which file to edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub file: PathBuf,
    pub section: String,
    pub key: String,
    pub line: usize,
    /// Whether this key reached the command line. A `[common]` key the current
    /// subcommand does not accept is carried for `frost doctor` to report and
    /// deliberately not spliced in.
    pub applies: bool,
}

/// The arguments a config file contributes, and where each came from.
#[derive(Debug, Default, Clone)]
pub struct Resolved {
    pub args: Vec<String>,
    pub origins: Vec<Origin>,
    /// Files that were read, in precedence order, for `doctor` to list.
    pub files: Vec<PathBuf>,
}

/// User config path. `~/.config/frost/frostrc`, or `$XDG_CONFIG_HOME`.
pub fn user_config_path() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg).join("frost").join("frostrc"));
    }
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".config")
            .join("frost")
            .join("frostrc")
    })
}

/// Read user then workspace config, and turn the sections that apply to
/// `subcommand` into arguments.
///
/// `configs` names `[config.NAME]` sections, applied in the order given. One
/// level only: a named section does not pull in another, because a config
/// language that can reference itself is the thing docs/14 declined.
pub fn resolve(
    root: &Path,
    subcommand: &str,
    configs: &[String],
    accepts: &dyn Fn(&str) -> bool,
) -> Result<Resolved> {
    let mut resolved = Resolved::default();
    let candidates = user_config_path()
        .into_iter()
        .chain(std::iter::once(root.join(WORKSPACE_FILE)));
    for path in candidates {
        if !path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        apply_file(&path, &text, subcommand, configs, accepts, &mut resolved)?;
        resolved.files.push(path);
    }
    Ok(resolved)
}

/// Merge one file's applicable sections into `resolved`.
///
/// Section order is fixed rather than file order: `[common]`, then the
/// subcommand's own section, then each `--config` in the order it was given.
/// A later section wins, which is what makes `--config ci` able to override a
/// workspace default rather than depending on where it sits in the file.
fn apply_file(
    path: &Path,
    text: &str,
    subcommand: &str,
    configs: &[String],
    accepts: &dyn Fn(&str) -> bool,
    resolved: &mut Resolved,
) -> Result<()> {
    let document: toml::Table =
        toml::from_str(text).with_context(|| format!("failed to parse {}", path.display()))?;

    let mut sections: Vec<String> = vec![COMMON.to_string(), subcommand.to_string()];
    sections.extend(configs.iter().map(|name| format!("config.{name}")));

    for section in sections {
        let Some(table) = lookup_section(&document, &section) else {
            continue;
        };
        let toml::Value::Table(table) = table else {
            bail!(
                "{}: [{section}] must be a table of option names",
                path.display()
            );
        };
        for (key, value) in table {
            if key.contains('.') {
                // `[config.ci]` arrives here as a nested table, not as a key.
                continue;
            }
            let line = line_of(text, &section, key);
            // `[common]` means "wherever it applies". `jobs` is the obvious
            // thing to put there and `frost doctor` has no `--jobs`, so
            // splicing it in regardless made the most natural config file break
            // half the subcommands. A key no subcommand accepts is still
            // refused, by `validate` — that is the typo case, and it is a
            // different question from this one.
            let applies = section == subcommand || accepts(key);
            if applies {
                resolved
                    .args
                    .extend(to_args(path, &section, key, value.clone(), line)?);
            }
            resolved.origins.push(Origin {
                file: path.to_path_buf(),
                section: section.clone(),
                key: key.clone(),
                line,
                applies,
            });
        }
    }
    Ok(())
}

fn lookup_section<'a>(document: &'a toml::Table, section: &str) -> Option<&'a toml::Value> {
    match section.split_once('.') {
        Some((outer, inner)) => document.get(outer)?.as_table()?.get(inner),
        None => document.get(section),
    }
}

/// `jobs = 16` into `["--jobs", "16"]`.
///
/// A boolean is a flag with no value, and `false` contributes nothing rather
/// than `--flag false`, which clap would reject. An array repeats the option,
/// which is how a repeatable one is spelled.
fn to_args(
    path: &Path,
    section: &str,
    key: &str,
    value: toml::Value,
    line: usize,
) -> Result<Vec<String>> {
    let flag = format!("--{}", key.replace('_', "-"));
    Ok(match value {
        toml::Value::Boolean(true) => vec![flag],
        // Not an error: writing `sandbox = false` to turn off a workspace
        // default is the obvious thing to try, and it does exactly what it
        // says, because the flag simply is not added.
        toml::Value::Boolean(false) => Vec::new(),
        toml::Value::String(text) => vec![flag, text],
        toml::Value::Integer(number) => vec![flag, number.to_string()],
        toml::Value::Array(items) => {
            let mut args = Vec::new();
            for item in items {
                match item {
                    toml::Value::String(text) => args.extend([flag.clone(), text]),
                    toml::Value::Integer(number) => args.extend([flag.clone(), number.to_string()]),
                    other => bail!(
                        "{}:{line}: [{section}] {key} has an array entry of type {}; \
                         only strings and integers become arguments",
                        path.display(),
                        other.type_str()
                    ),
                }
            }
            args
        }
        other => bail!(
            "{}:{line}: [{section}] {key} is a {}; \
             an option value must be a string, integer, boolean or array",
            path.display(),
            other.type_str()
        ),
    })
}

/// Best-effort line number for a key inside a section.
///
/// `toml` does not hand back spans for a plain `Table`, and a diagnostic that
/// says "somewhere in this file" is most of the way to useless on a config
/// file someone else wrote. Scanning for the section header and then the key
/// is exact for the shape these files actually have, and falls back to the
/// section's own line rather than lying.
fn line_of(text: &str, section: &str, key: &str) -> usize {
    let header = format!("[{section}]");
    let mut in_section = false;
    let mut section_line = 0;
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.starts_with('[') {
            in_section = line == header;
            if in_section {
                section_line = index + 1;
            }
            continue;
        }
        if in_section {
            let name = line.split('=').next().unwrap_or("").trim();
            if name == key {
                return index + 1;
            }
        }
    }
    section_line
}

/// Reject keys no subcommand accepts, naming the file, line and key.
///
/// Checked against clap's own argument tree rather than a hand-kept list, so a
/// new option is accepted in a config file the moment it exists on the command
/// line, and a removed one stops being accepted at the same moment.
pub fn validate(command: &clap::Command, subcommand: &str, resolved: &Resolved) -> Result<()> {
    let known = |name: &str| -> bool {
        let long = name.replace('_', "-");
        let matches = |cmd: &clap::Command| {
            cmd.get_arguments()
                .any(|arg| arg.get_long() == Some(long.as_str()))
        };
        matches(command)
            || command
                .get_subcommands()
                .any(|sub| sub.get_name() == subcommand && matches(sub))
    };
    // Two different questions, and conflating them is what made `[common]
    // jobs` break `frost doctor`:
    //
    // - a key in a subcommand's own section must be an option of *that*
    //   subcommand, because naming the section is naming the command;
    // - a key in `[common]` or a `[config.*]` set must be an option of *some*
    //   subcommand. It is applied where it fits and skipped where it does not,
    //   which is what "common" has to mean to be worth writing.
    //
    // Either way a key no subcommand accepts anywhere is a typo and is
    // refused, which is the promise in docs/06.
    let known_anywhere = |name: &str| -> bool {
        let long = name.replace('_', "-");
        let matches = |cmd: &clap::Command| {
            cmd.get_arguments()
                .any(|arg| arg.get_long() == Some(long.as_str()))
        };
        matches(command) || command.get_subcommands().any(matches)
    };
    for origin in &resolved.origins {
        let accepted = match origin.section == subcommand {
            true => known(&origin.key),
            false => known_anywhere(&origin.key),
        };
        if !accepted {
            let hint = nearest_option(command, subcommand, &origin.key)
                .map(|name| format!(", did you mean `{name}`?"))
                .unwrap_or_default();
            bail!(
                "{}:{}: [{}] {} is not an option of `frost {subcommand}`{hint}",
                origin.file.display(),
                origin.line,
                origin.section,
                origin.key,
            );
        }
    }
    Ok(())
}

fn nearest_option(command: &clap::Command, subcommand: &str, key: &str) -> Option<String> {
    let mut names: Vec<String> = command
        .get_arguments()
        .filter_map(|arg| arg.get_long().map(str::to_string))
        .collect();
    if let Some(sub) = command
        .get_subcommands()
        .find(|sub| sub.get_name() == subcommand)
    {
        names.extend(
            sub.get_arguments()
                .filter_map(|arg| arg.get_long().map(str::to_string)),
        );
    }
    let candidates: Vec<&str> = names.iter().map(String::as_str).collect();
    frostbuild_core::manifest::closest(&key.replace('_', "-"), candidates.iter().copied())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved_of(text: &str, subcommand: &str, configs: &[&str]) -> Resolved {
        let mut resolved = Resolved::default();
        let owned: Vec<String> = configs.iter().map(|s| s.to_string()).collect();
        apply_file(
            Path::new("/w/.frostrc"),
            text,
            subcommand,
            &owned,
            &|_| true,
            &mut resolved,
        )
        .unwrap();
        resolved
    }

    #[test]
    fn common_then_subcommand_then_each_named_config_in_order() {
        // The order is the precedence, because clap takes the last occurrence.
        let text = r#"
[common]
jobs = 4

[build]
jobs = 8

[config.ci]
jobs = 16

[config.gpu]
jobs = 32
"#;
        let resolved = resolved_of(text, "build", &["ci", "gpu"]);
        assert_eq!(
            resolved.args,
            ["--jobs", "4", "--jobs", "8", "--jobs", "16", "--jobs", "32"]
        );

        // Declaration order on the command line decides, not order in the file.
        let resolved = resolved_of(text, "build", &["gpu", "ci"]);
        assert_eq!(
            resolved.args,
            ["--jobs", "4", "--jobs", "8", "--jobs", "32", "--jobs", "16"]
        );
    }

    #[test]
    fn a_section_for_another_subcommand_is_not_applied() {
        let text = "[build]\njobs = 8\n\n[test]\njobs = 2\n";
        assert_eq!(resolved_of(text, "build", &[]).args, ["--jobs", "8"]);
        assert_eq!(resolved_of(text, "test", &[]).args, ["--jobs", "2"]);
    }

    #[test]
    fn value_shapes_become_the_arguments_they_look_like() {
        let text = "[build]\nsandbox = true\nprofile = \"release\"\njobs = 16\n";
        let args = resolved_of(text, "build", &[]).args;
        assert!(args.contains(&"--sandbox".to_string()));
        assert!(args.windows(2).any(|w| w == ["--profile", "release"]));
        assert!(args.windows(2).any(|w| w == ["--jobs", "16"]));

        // `false` contributes nothing. Turning off a workspace default is the
        // obvious thing to try, and `--flag false` is not a thing clap accepts.
        let args = resolved_of("[build]\nsandbox = false\n", "build", &[]).args;
        assert!(args.is_empty(), "{args:?}");

        // An underscore in TOML is the hyphen in the option, so a file can be
        // written either way.
        let args = resolved_of("[build]\nno_tui = true\n", "build", &[]).args;
        assert_eq!(args, ["--no-tui"]);
    }

    #[test]
    fn an_array_repeats_the_option() {
        let args = resolved_of("[test]\ntest_arg = [\"-v\", \"-q\"]\n", "test", &[]).args;
        assert_eq!(args, ["--test-arg", "-v", "--test-arg", "-q"]);
    }

    #[test]
    fn a_key_is_reported_with_the_line_it_is_on() {
        let text = "[common]\njobs = 4\n\n[build]\nprofile = \"release\"\n";
        let resolved = resolved_of(text, "build", &[]);
        let profile = resolved
            .origins
            .iter()
            .find(|origin| origin.key == "profile")
            .expect("origin recorded");
        assert_eq!(profile.line, 5);
        assert_eq!(profile.section, "build");
    }

    #[test]
    fn a_value_of_the_wrong_shape_names_the_file_line_and_key() {
        let error = apply_file(
            Path::new("/w/.frostrc"),
            "[build]\njobs = { a = 1 }\n",
            "build",
            &[],
            &|_| true,
            &mut Resolved::default(),
        )
        .unwrap_err();
        let text = format!("{error:#}");
        assert!(text.contains("/w/.frostrc:2"), "{text}");
        assert!(text.contains("jobs"), "{text}");
        assert!(text.contains("table"), "{text}");
    }
}
