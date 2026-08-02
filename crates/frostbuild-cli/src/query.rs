//! `frost query`: the graph as questions — deps, rdeps, paths, kinds, attributes.

use std::collections::BTreeSet;

use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use frostbuild_core::graph::BuildGraph;
use frostbuild_core::manifest::TargetKind;

use crate::cli::{QueryCmd, QueryOpts, QueryOutput};
use crate::graph::load_graph;

impl QueryCmd {
    fn opts(&self) -> &QueryOpts {
        match self {
            QueryCmd::Deps { opts, .. }
            | QueryCmd::Rdeps { opts, .. }
            | QueryCmd::Somepath { opts, .. }
            | QueryCmd::Allpaths { opts, .. }
            | QueryCmd::Targets { opts }
            | QueryCmd::Owners { opts, .. } => opts,
        }
    }
}

impl QueryOutput {
    fn as_str(self) -> &'static str {
        match self {
            QueryOutput::Text => "text",
            QueryOutput::Json => "json",
            QueryOutput::LabelKind => "label-kind",
            QueryOutput::Dot => "dot",
        }
    }
}

impl QueryOpts {
    /// `--json` predates `--output`, so it stays and means `--output json`.
    /// Two spellings that disagree are a mistake worth naming rather than
    /// silently resolving in one direction.
    fn format(&self) -> Result<QueryOutput> {
        match (self.output, self.json) {
            (Some(QueryOutput::Json), _) | (None, true) => Ok(QueryOutput::Json),
            (Some(other), true) => bail!(
                "--json and --output {} disagree; --json is the older spelling of --output json",
                other.as_str()
            ),
            (Some(other), false) => Ok(other),
            (None, false) => Ok(QueryOutput::Text),
        }
    }
}

/// One `--attr NAME=PATTERN` restriction. The set is closed: an unrecognized
/// name is a typo that would otherwise silently widen the result.
pub(crate) enum AttrFilter {
    Deps(frostbuild_core::graph::PathPattern),
    Srcs(frostbuild_core::graph::PathPattern),
    Outputs(frostbuild_core::graph::PathPattern),
    Sandbox(bool),
    Timeout(Option<u64>),
}

impl AttrFilter {
    pub(crate) const NAMES: [&'static str; 5] = ["deps", "srcs", "outputs", "sandbox", "timeout"];

    fn parse(spec: &str) -> Result<Self> {
        let (name, value) = spec
            .split_once('=')
            .with_context(|| format!("--attr {spec:?} is not NAME=PATTERN"))?;
        let pattern = || frostbuild_core::graph::PathPattern::new(value);
        match name {
            "deps" => Ok(AttrFilter::Deps(pattern()?)),
            "srcs" => Ok(AttrFilter::Srcs(pattern()?)),
            "outputs" => Ok(AttrFilter::Outputs(pattern()?)),
            "sandbox" => match value {
                "true" => Ok(AttrFilter::Sandbox(true)),
                "false" => Ok(AttrFilter::Sandbox(false)),
                other => bail!("--attr sandbox= takes true or false, not {other:?}"),
            },
            "timeout" => match value {
                "none" => Ok(AttrFilter::Timeout(None)),
                other => Ok(AttrFilter::Timeout(Some(other.parse().with_context(
                    || format!("--attr timeout= takes seconds or none, not {other:?}"),
                )?))),
            },
            other => bail!(
                "unknown --attr name {other:?}; expected one of {}",
                Self::NAMES.join(", ")
            ),
        }
    }

    fn matches(&self, graph: &BuildGraph, name: &str) -> bool {
        let Some(target) = graph.targets.get(name) else {
            return false;
        };
        let file = |id: &usize| graph.files[*id].path.as_str();
        match self {
            AttrFilter::Deps(pattern) => target.deps.iter().any(|dep| pattern.matches(dep)),
            // Declared inputs only. Order-only generated headers are reachable
            // through `query owners`, but they are not this target's sources.
            AttrFilter::Srcs(pattern) => target
                .actions
                .iter()
                .flat_map(|a| graph.actions[*a].inputs.iter())
                .any(|id| pattern.matches(file(id))),
            AttrFilter::Outputs(pattern) => target
                .outputs
                .iter()
                .chain(
                    target
                        .actions
                        .iter()
                        .flat_map(|a| graph.actions[*a].outputs.iter()),
                )
                .any(|id| pattern.matches(file(id))),
            AttrFilter::Sandbox(want) => target
                .actions
                .iter()
                .any(|a| graph.actions[*a].sandbox == *want),
            AttrFilter::Timeout(want) => target.timeout_secs == *want,
        }
    }
}

/// Answer a graph question without configuring a build.
///
/// Every function here is configuration-free: the target-level graph has
/// unconditional deps, so any profile or platform yields the same shape and
/// the answer does not depend on how the caller would have built it.
pub(crate) fn run_query(root: &std::path::Path, function: &QueryCmd) -> Result<i32> {
    let graph = load_graph(root, "debug", frostbuild_core::manifest::HOST_PLATFORM)?;
    let opts = function.opts();
    let format = opts.format()?;
    if let Some(kind) = &opts.kind {
        if !TargetKind::ALL.iter().any(|k| k.as_str() == kind) {
            let known: Vec<&str> = TargetKind::ALL.iter().map(|k| k.as_str()).collect();
            bail!(
                "unknown --kind {kind:?}; expected one of {}",
                known.join(", ")
            );
        }
    }
    let attrs = opts
        .attr
        .iter()
        .map(|spec| AttrFilter::parse(spec))
        .collect::<Result<Vec<_>>>()?;
    let keep = |name: &String| {
        let kind_ok = opts.kind.as_ref().is_none_or(|want| {
            graph
                .targets
                .get(name)
                .is_some_and(|target| target.kind.as_str() == want)
        });
        kind_ok && attrs.iter().all(|attr| attr.matches(&graph, name))
    };

    let mut paths: Option<Vec<Vec<String>>> = None;
    let mut truncated = false;
    let (query, targets) = match function {
        QueryCmd::Deps { target, .. } => (format!("deps({target})"), graph.deps_closure(target)?),
        QueryCmd::Rdeps { target, .. } => {
            (format!("rdeps({target})"), graph.rdeps_closure(target)?)
        }
        QueryCmd::Somepath { from, to, .. } => {
            let Some(path) = graph.somepath(from, to)? else {
                println!("no path from {from} to {to}");
                return Ok(1);
            };
            (format!("somepath({from}, {to})"), path)
        }
        QueryCmd::Allpaths {
            from, to, limit, ..
        } => {
            let found = graph.allpaths(from, to, *limit)?;
            if found.paths.is_empty() {
                println!("no path from {from} to {to}");
                return Ok(1);
            }
            truncated = found.truncated;
            // A filter drops members from each path rather than dropping the
            // path: "the test targets along this route" is still a route.
            let kept: Vec<Vec<String>> = found
                .paths
                .into_iter()
                .map(|path| path.into_iter().filter(&keep).collect::<Vec<_>>())
                .filter(|path| !path.is_empty())
                .collect();
            let union: BTreeSet<String> = kept.iter().flatten().cloned().collect();
            paths = Some(kept);
            (
                format!("allpaths({from}, {to})"),
                union.into_iter().collect(),
            )
        }
        QueryCmd::Owners { paths: files, .. } => (
            format!("owners({})", files.join(", ")),
            graph.owners(files)?,
        ),
        // Already sorted: `targets` is a BTreeMap, and a listing whose order
        // changed between runs would be useless to diff.
        QueryCmd::Targets { .. } => (
            "targets()".to_string(),
            graph.targets.keys().cloned().collect(),
        ),
    };
    let targets: Vec<String> = targets.into_iter().filter(keep).collect();

    match format {
        QueryOutput::Json => {
            let mut payload = serde_json::json!({ "query": query, "targets": targets });
            if let Some(paths) = &paths {
                payload["paths"] = serde_json::json!(paths);
                payload["truncated"] = serde_json::json!(truncated);
            }
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        QueryOutput::Text => match &paths {
            // One path per block keeps the existing one-target-per-line shape
            // that `deps`, `rdeps` and `somepath` have always printed.
            Some(paths) => {
                for (index, path) in paths.iter().enumerate() {
                    if index > 0 {
                        println!();
                    }
                    for name in path {
                        println!("{name}");
                    }
                }
            }
            None => {
                for name in &targets {
                    println!("{name}");
                }
            }
        },
        QueryOutput::LabelKind => {
            for name in &targets {
                let kind = graph
                    .targets
                    .get(name)
                    .map_or("unknown", |target| target.kind.as_str());
                println!("{kind} target {name}");
            }
        }
        QueryOutput::Dot => {
            let selected: BTreeSet<&str> = targets.iter().map(String::as_str).collect();
            println!("digraph frost_query {{");
            println!("  rankdir=LR;");
            for name in &targets {
                println!("  {name:?};");
            }
            for name in &targets {
                let Some(target) = graph.targets.get(name) else {
                    continue;
                };
                let mut deps: Vec<&str> = target
                    .deps
                    .iter()
                    .map(String::as_str)
                    .filter(|dep| selected.contains(dep))
                    .collect();
                deps.sort_unstable();
                for dep in deps {
                    println!("  {name:?} -> {dep:?};");
                }
            }
            println!("}}");
        }
    }

    if truncated && format != QueryOutput::Json {
        eprintln!("frost: stopped at the --limit; more paths exist and this list is not complete");
    }
    if targets.is_empty() {
        // stdout stays empty so a pipeline sees nothing, but a person running
        // this by hand should not have to guess whether the query matched
        // nothing or the filter removed everything.
        let reason = if opts.kind.is_some() || !opts.attr.is_empty() {
            "nothing matched, or the --kind/--attr filters removed everything"
        } else if matches!(function, QueryCmd::Owners { .. }) {
            "no target declares those paths among its action inputs; a header \
             read only through a depfile is build state, so `frost explain` \
             reports it instead"
        } else {
            "nothing matched"
        };
        eprintln!("frost: {query}: {reason}");
        return Ok(1);
    }
    Ok(0)
}
