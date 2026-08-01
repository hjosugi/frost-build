//! Who may depend on a target.
//!
//! Multi-package labels made it possible to split a workspace into modules.
//! Nothing yet makes those modules mean anything: any target may depend on any
//! other, so the boundary exists only in whatever discipline the team keeps,
//! and the first deadline erases it. A build tool that knows the whole graph is
//! the only thing positioned to enforce it, and enforcing it costs one check at
//! load.
//!
//! **The default is public**, which is not what Bazel does and is deliberate.
//! Frost has workspaces in the world already; a private-by-default rule would
//! break every one of them on upgrade, and a correctness feature that arrives
//! as a wall of errors is one people turn off. `frost lint` names the targets
//! where the boundary actually matters — the ones already depended on from
//! another package — so the migration is a list you can work through rather
//! than a flag day.
//!
//! Not action-key material: visibility says who may *ask* for a target, not
//! what building it produces. Two workspaces differing only in visibility
//! build byte-identical outputs, and must share cache entries.

use std::collections::BTreeMap;

use anyhow::{bail, Result};

/// Every package may depend on this target. Written out because it is also the
/// spelling of the default, and a workspace should be able to say so.
pub const PUBLIC: &str = "//...";

/// A named list declared once in the root manifest, referenced as
/// `group:NAME`. The prefix rather than a new sigil: `//` already means a
/// label, and a bare name would be ambiguous with a single-manifest target.
const GROUP_PREFIX: &str = "group:";

/// The subtree suffix, as in `//apps/...`.
const SUBTREE_SUFFIX: &str = "/...";

/// One entry of a `visibility` list, already parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    /// `//...`
    Everywhere,
    /// `//apps/...` — that package and everything under it.
    Subtree(String),
    /// `//apps/cli:cli` — one target.
    Target(String),
    /// `group:internal`
    Group(String),
}

impl Rule {
    /// Parse one entry. `groups` is only needed to reject a name nobody
    /// declared, which is a typo every time and would otherwise silently deny.
    pub fn parse(entry: &str, groups: &BTreeMap<String, Vec<Rule>>) -> Result<Self> {
        if entry == PUBLIC {
            return Ok(Self::Everywhere);
        }
        if let Some(name) = entry.strip_prefix(GROUP_PREFIX) {
            if !groups.contains_key(name) {
                let known: Vec<&str> = groups.keys().map(String::as_str).collect();
                // Either a misspelling or a group nobody wrote — different
                // mistakes with different fixes. Printing both, as an earlier
                // draft did, told the reader to declare the name it had just
                // said was wrong.
                match crate::manifest::closest(name, known.iter().copied()) {
                    Some(other) => {
                        bail!("visibility names undeclared group {name:?}, did you mean {other:?}?")
                    }
                    None => bail!(
                        "visibility names undeclared group {name:?}; \
                         declare it as [visibility.{name}] in the root manifest"
                    ),
                }
            }
            return Ok(Self::Group(name.to_string()));
        }
        let Some(rest) = entry.strip_prefix("//") else {
            bail!(
                "visibility entry {entry:?} must be {PUBLIC:?}, //package/..., \
                 //package:target, or group:NAME"
            );
        };
        if let Some(package) = rest.strip_suffix(SUBTREE_SUFFIX) {
            if package.is_empty() {
                // `///...` and friends. `//...` is the spelling for everywhere.
                bail!("visibility entry {entry:?} names an empty package; use {PUBLIC:?}");
            }
            return Ok(Self::Subtree(package.to_string()));
        }
        if rest.contains(':') {
            return Ok(Self::Target(entry.to_string()));
        }
        // `//apps` on its own reads like "the apps package", which is one
        // character away from `//apps/...` and means something different. Ask
        // rather than guess.
        bail!(
            "visibility entry {entry:?} names a package without saying what in it: \
             write {entry}/... for the subtree, or {entry}:target for one target"
        )
    }

    /// Does this rule admit a dependent in `package`, named `label`?
    fn admits(&self, package: &str, label: &str, groups: &BTreeMap<String, Vec<Rule>>) -> bool {
        match self {
            Self::Everywhere => true,
            Self::Subtree(prefix) => in_subtree(package, prefix),
            Self::Target(other) => other == label,
            // One level: a group holds rules, not other groups. `parse`
            // accepts `group:` inside a group, so guard against a cycle by
            // refusing to follow one — the manifest validation below reports
            // it properly.
            Self::Group(name) => groups.get(name).is_some_and(|rules| {
                rules.iter().any(|rule| {
                    !matches!(rule, Self::Group(_)) && rule.admits(package, label, groups)
                })
            }),
        }
    }
}

/// Is `package` inside `prefix`, comparing whole segments?
///
/// Segment-wise so `//app` does not admit `//apple`, which is the same class of
/// bug as a path prefix check that matches `dist2` inside `dist`.
fn in_subtree(package: &str, prefix: &str) -> bool {
    package == prefix
        || (package.len() > prefix.len()
            && package.starts_with(prefix)
            && package.as_bytes()[prefix.len()] == b'/')
}

/// May `dependent` (in `dependent_package`) depend on a target whose
/// visibility list is `rules`?
///
/// A target is always visible inside its own package: a package is the unit
/// people already treat as one thing, and requiring a declaration to use your
/// own neighbour would make the feature nothing but noise.
pub fn admits(
    rules: &[Rule],
    dependent: &str,
    dependent_package: &str,
    target_package: &str,
    groups: &BTreeMap<String, Vec<Rule>>,
) -> bool {
    if dependent_package == target_package {
        return true;
    }
    rules
        .iter()
        .any(|rule| rule.admits(dependent_package, dependent, groups))
}

/// How a rule reads in an error message.
pub fn describe(rules: &[Rule]) -> String {
    if rules.is_empty() {
        return "nothing outside its own package".to_string();
    }
    rules
        .iter()
        .map(|rule| match rule {
            Rule::Everywhere => PUBLIC.to_string(),
            Rule::Subtree(package) => format!("//{package}/..."),
            Rule::Target(label) => label.clone(),
            Rule::Group(name) => format!("group:{name}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups() -> BTreeMap<String, Vec<Rule>> {
        BTreeMap::from([(
            "internal".to_string(),
            vec![Rule::Subtree("core".into()), Rule::Subtree("text".into())],
        )])
    }

    fn parse(entry: &str) -> Rule {
        Rule::parse(entry, &groups()).unwrap()
    }

    #[test]
    fn each_spelling_parses_to_the_rule_it_reads_as() {
        assert_eq!(parse("//..."), Rule::Everywhere);
        assert_eq!(parse("//apps/..."), Rule::Subtree("apps".into()));
        assert_eq!(
            parse("//apps/cli:cli"),
            Rule::Target("//apps/cli:cli".into())
        );
        assert_eq!(parse("group:internal"), Rule::Group("internal".into()));
    }

    #[test]
    fn a_package_without_a_subtree_or_target_is_refused() {
        // `//apps` is one character from `//apps/...` and means something
        // different. Guessing which was meant is how a boundary silently opens.
        let error = Rule::parse("//apps", &groups()).unwrap_err().to_string();
        assert!(error.contains("//apps/..."), "{error}");
        assert!(error.contains("//apps:target"), "{error}");

        let error = Rule::parse("apps/...", &groups()).unwrap_err().to_string();
        assert!(error.contains("must be"), "{error}");
    }

    #[test]
    fn an_undeclared_group_is_a_typo_rather_than_a_denial() {
        // Treating it as "matches nothing" would deny the dependency and send
        // the reader looking at the wrong target entirely.
        let error = Rule::parse("group:internl", &groups())
            .unwrap_err()
            .to_string();
        assert!(error.contains("did you mean \"internal\""), "{error}");
        // …and it must not also say "declare [visibility.internl]", which is
        // advice to create the misspelling it just diagnosed.
        assert!(!error.contains("declare it"), "{error}");

        // With nothing close, declaring it *is* the fix.
        let error = Rule::parse("group:unrelated", &BTreeMap::new())
            .unwrap_err()
            .to_string();
        assert!(error.contains("[visibility.unrelated]"), "{error}");
    }

    #[test]
    fn a_subtree_matches_whole_segments_only() {
        assert!(in_subtree("apps", "apps"));
        assert!(in_subtree("apps/cli", "apps"));
        // The bug this exists to prevent: a prefix match would let `//app/...`
        // admit everything in `apple`.
        assert!(!in_subtree("apple", "app"));
        assert!(!in_subtree("apps", "apps/cli"));
    }

    #[test]
    fn a_target_is_visible_inside_its_own_package_without_saying_so() {
        assert!(admits(&[], "//core:helper", "core", "core", &groups()));
        assert!(!admits(
            &[],
            "//apps/cli:cli",
            "apps/cli",
            "core",
            &groups()
        ));
    }

    #[test]
    fn a_group_admits_what_its_members_admit() {
        let rules = vec![parse("group:internal")];
        assert!(admits(&rules, "//core:core", "core", "render", &groups()));
        assert!(admits(&rules, "//text:text", "text", "render", &groups()));
        assert!(!admits(
            &rules,
            "//apps/cli:cli",
            "apps/cli",
            "render",
            &groups()
        ));
    }

    #[test]
    fn a_group_that_names_a_group_admits_nothing_through_it() {
        // One level, per the issue. The manifest rejects the declaration, so
        // this only pins that the matcher cannot recurse if one slipped past.
        let nested = BTreeMap::from([("a".to_string(), vec![Rule::Group("b".into())])]);
        assert!(!admits(
            &[Rule::Group("a".into())],
            "//x:y",
            "x",
            "z",
            &nested
        ));
    }

    #[test]
    fn describing_the_rule_is_what_the_error_message_shows() {
        assert_eq!(describe(&[]), "nothing outside its own package");
        assert_eq!(
            describe(&[parse("//apps/..."), parse("//core:core")]),
            "//apps/..., //core:core"
        );
    }
}
