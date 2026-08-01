//! `--report`: one build, explained in one file.
//!
//! `--stats` has the counters, `--trace` has the timeline and `explain` has the
//! reasons, and none of the three is the answer to "why was this build slow" on
//! its own. A Chrome trace is the closest, and it asks the reader to open
//! `chrome://tracing` first, which makes it a poor thing to hand to a
//! colleague. Gradle's build scan is the shape that works — one build, one
//! link, timeline and cache breakdown and failures on one screen — minus the
//! hosted service that is also its adoption barrier.
//!
//! So: one HTML file, no server, no network, no JavaScript. Every number in it
//! is already produced by the build — the scheduler's critical path, the
//! journal's durations, the same invalidation reasons `--explain` prints. The
//! report is a rendering, not a measurement, and it is written after the build
//! has been timed and summarized, so it cannot move the number it reports.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use frostbuild_core::graph::ActionKind;
use frostbuild_exec::{BuildReport, Outcome};

/// How many executed actions the "slowest" table lists. Enough to find the one
/// that dominated, short enough to read without scrolling past the sections
/// below it.
const SLOWEST: usize = 20;

/// How many lines of a failed action's output the report carries. The end is
/// the part that says what happened; the beginning is usually the command line.
const FAILURE_TAIL_LINES: usize = 40;

/// Where `--report` writes when it is given no path.
///
/// Under `.frost/`, which frost owns and workspaces already ignore: a build
/// flag should not drop a file into a source tree by default.
pub fn default_path(profile: &str, platform: &str) -> PathBuf {
    PathBuf::from(".frost/report").join(format!("{platform}-{profile}.html"))
}

/// What the report renders. Every field is data the build already produced.
pub struct Build<'a> {
    pub workspace: &'a str,
    pub profile: &'a str,
    pub platform: &'a str,
    pub targets: &'a [String],
    pub report: &'a BuildReport,
    /// Actions in the whole graph, against which the closure is a subset.
    pub graph_actions: usize,
    pub elapsed_ms: u128,
    /// The `--trace` destination, when this build also wrote one.
    pub trace: Option<&'a Path>,
    pub test_mode: bool,
}

/// Render `build` to `destination`, creating its parent directory.
pub fn write(destination: &Path, build: &Build) -> Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(destination, render(destination, build))
        .with_context(|| format!("writing {}", destination.display()))
}

fn render(destination: &Path, build: &Build) -> String {
    let mut html = String::with_capacity(16 * 1024);
    let verb = if build.test_mode { "test" } else { "build" };
    let title = format!("frost {verb} · {}", build.workspace);

    html.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n");
    html.push_str("<meta charset=\"utf-8\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    let _ = writeln!(html, "<title>{}</title>", escape(&title));
    html.push_str(STYLE);
    html.push_str("</head>\n<body>\n");

    let _ = writeln!(html, "<h1>frost {verb}</h1>");
    let _ = writeln!(
        html,
        "<p class=\"sub\">{} · {} / {} · {}</p>",
        escape(build.workspace),
        escape(build.profile),
        escape(build.platform),
        escape(&describe_targets(build.targets)),
    );

    summary(&mut html, build);
    critical_path(&mut html, build);
    slowest(&mut html, build);
    by_kind(&mut html, build);
    reasons(&mut html, build);
    if build.test_mode || build.report.results.iter().any(is_test) {
        tests(&mut html, build);
    }
    failures(&mut html, build);
    footer(&mut html, destination, build);

    html.push_str("</body>\n</html>\n");
    html
}

fn summary(html: &mut String, build: &Build) {
    let report = build.report;
    let stats = &report.stats;
    let executed = report.executed();
    let cached = report.cached();
    let failed = report.failed();
    let skipped = report.count(|outcome| matches!(outcome, Outcome::Skipped { .. }));
    let total = report.results.len();

    html.push_str("<section class=\"tiles\">\n");
    tile(html, "wall time", &format!("{} ms", build.elapsed_ms), None);
    tile(
        html,
        "actions",
        &total.to_string(),
        Some(&format!("of {} in the graph", build.graph_actions)),
    );
    tile(html, "ran", &executed.to_string(), None);
    tile(
        html,
        "cached",
        &cached.to_string(),
        (total > 0)
            .then(|| {
                format!(
                    "{:.0}% of the closure",
                    100.0 * cached as f64 / total as f64
                )
            })
            .as_deref(),
    );
    if failed > 0 {
        tile(html, "failed", &failed.to_string(), None);
    }
    if skipped > 0 {
        tile(html, "skipped", &skipped.to_string(), None);
    }
    html.push_str("</section>\n");

    // Scheduling has nothing to describe when nothing ran, and printing
    // "0 ms across 0 actions, 0% utilization" reads like a malfunction.
    if stats.executed == 0 {
        html.push_str(
            "<p class=\"note\">Nothing ran, so there was nothing to schedule. \
             Every action in the closure was already recorded as current.</p>\n",
        );
        return;
    }
    html.push_str("<section class=\"tiles\">\n");
    tile(
        html,
        "makespan",
        &format!("{} ms", stats.makespan_ms),
        Some(&format!("{} ms of work", stats.busy_ms)),
    );
    tile(
        html,
        "utilization",
        &format!("{:.0}%", stats.utilization_pct()),
        Some(&format!("of {} workers", stats.jobs)),
    );
    tile(
        html,
        "strategy",
        stats.scheduler,
        Some(&format!("{} estimator", stats.estimator)),
    );
    html.push_str("</section>\n");
}

fn critical_path(html: &mut String, build: &Build) {
    let path = &build.report.critical_path;
    if path.is_empty() {
        return;
    }
    let by_id: BTreeMap<&str, &frostbuild_exec::ActionResult> = build
        .report
        .results
        .iter()
        .map(|result| (result.id.as_str(), result))
        .collect();

    html.push_str("<section>\n<h2>Critical path</h2>\n");
    html.push_str(
        "<p class=\"note\">The longest chain of dependent actions. \
         No number of workers makes a build shorter than this chain, so it is \
         the only place where making one action faster shortens the build.</p>\n",
    );
    let estimated = build.report.stats.critical_path_ms;
    let measured: u64 = path
        .iter()
        .filter_map(|id| by_id.get(id.as_str()))
        .filter_map(|result| match result.outcome {
            Outcome::Executed { duration_ms, .. } => Some(duration_ms),
            _ => None,
        })
        .sum();
    let _ = writeln!(
        html,
        "<p class=\"note\">{} actions · {estimated} ms estimated before the run · \
         {measured} ms actually spent on this chain</p>",
        path.len()
    );

    html.push_str(
        "<table>\n<thead><tr><th>#</th><th>action</th><th>target</th>\
                   <th class=\"num\">ms</th><th>outcome</th></tr></thead>\n<tbody>\n",
    );
    for (position, id) in path.iter().enumerate() {
        let Some(result) = by_id.get(id.as_str()) else {
            continue;
        };
        let _ = writeln!(
            html,
            "<tr><td class=\"num\">{}</td><td>{}</td><td class=\"dim\">{}</td>\
             <td class=\"num\">{}</td><td>{}</td></tr>",
            position + 1,
            escape(&result.desc),
            escape(&result.target),
            duration_cell(&result.outcome),
            outcome_cell(&result.outcome),
        );
    }
    html.push_str("</tbody>\n</table>\n</section>\n");
}

fn slowest(html: &mut String, build: &Build) {
    let mut ran: Vec<(u64, &frostbuild_exec::ActionResult)> = build
        .report
        .results
        .iter()
        .filter_map(|result| match result.outcome {
            Outcome::Executed { duration_ms, .. } => Some((duration_ms, result)),
            _ => None,
        })
        .collect();
    if ran.is_empty() {
        return;
    }
    // Longest first; ties keep deterministic graph order, so two runs of the
    // same build produce the same table.
    ran.sort_by_key(|(duration_ms, _)| std::cmp::Reverse(*duration_ms));
    let shown = ran.len().min(SLOWEST);
    let total: u64 = ran.iter().map(|(ms, _)| ms).sum();

    html.push_str("<section>\n<h2>Slowest actions that ran</h2>\n");
    if ran.len() > shown {
        let _ = writeln!(
            html,
            "<p class=\"note\">Top {shown} of {} executed, {total} ms in total.</p>",
            ran.len()
        );
    } else {
        let _ = writeln!(
            html,
            "<p class=\"note\">All {shown} executed, {total} ms in total.</p>"
        );
    }
    html.push_str(
        "<table>\n<thead><tr><th>action</th><th>target</th><th class=\"num\">ms</th>\
         <th>why it ran</th></tr></thead>\n<tbody>\n",
    );
    for (duration_ms, result) in ran.iter().take(shown) {
        let reason = match &result.outcome {
            Outcome::Executed { reason, .. } => reason.as_str(),
            _ => "",
        };
        let _ = writeln!(
            html,
            "<tr><td>{}</td><td class=\"dim\">{}</td><td class=\"num\">{duration_ms}</td>\
             <td class=\"dim\">{}</td></tr>",
            escape(&result.desc),
            escape(&result.target),
            escape(reason),
        );
    }
    html.push_str("</tbody>\n</table>\n</section>\n");
}

#[derive(Default)]
struct KindTally {
    ran: usize,
    cached: usize,
    failed: usize,
    skipped: usize,
    ms: u64,
}

fn by_kind(html: &mut String, build: &Build) {
    let mut tallies: BTreeMap<&'static str, KindTally> = BTreeMap::new();
    for result in &build.report.results {
        let tally = tallies.entry(kind_name(result.kind)).or_default();
        match &result.outcome {
            Outcome::Executed { duration_ms, .. } => {
                tally.ran += 1;
                tally.ms += duration_ms;
            }
            Outcome::Cached => tally.cached += 1,
            Outcome::Failed { .. } => tally.failed += 1,
            Outcome::Skipped { .. } => tally.skipped += 1,
            // Dry runs do not write reports.
            Outcome::WouldRun { .. } | Outcome::MayRun { .. } => {}
        }
    }
    if tallies.is_empty() {
        return;
    }

    html.push_str("<section>\n<h2>Cache, by kind of work</h2>\n");
    html.push_str(
        "<p class=\"note\">A kind that never hits the cache is where to look \
         first: either its inputs really do change every time, or something \
         undeclared is being read.</p>\n",
    );
    html.push_str(
        "<table>\n<thead><tr><th>kind</th><th class=\"num\">ran</th>\
         <th class=\"num\">cached</th><th class=\"num\">failed</th>\
         <th class=\"num\">skipped</th><th class=\"num\">hit rate</th>\
         <th class=\"num\">ms</th></tr></thead>\n<tbody>\n",
    );
    for (kind, tally) in &tallies {
        // A cache hit is only meaningful against work that could have run.
        // Skipped actions never reached the question.
        let decided = tally.ran + tally.cached + tally.failed;
        let hit_rate = if decided == 0 {
            "—".to_string()
        } else {
            format!("{:.0}%", 100.0 * tally.cached as f64 / decided as f64)
        };
        let _ = writeln!(
            html,
            "<tr><td>{kind}</td><td class=\"num\">{}</td><td class=\"num\">{}</td>\
             <td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{hit_rate}</td>\
             <td class=\"num\">{}</td></tr>",
            tally.ran, tally.cached, tally.failed, tally.skipped, tally.ms
        );
    }
    html.push_str("</tbody>\n</table>\n</section>\n");
}

fn reasons(html: &mut String, build: &Build) {
    // Reasons carry a detail after ": " — which input changed, which output is
    // missing. Grouping on the part before it gives `--explain`'s vocabulary
    // rather than one row per file.
    let mut groups: BTreeMap<&str, (usize, Vec<&str>)> = BTreeMap::new();
    for result in &build.report.results {
        let Outcome::Executed { reason, .. } = &result.outcome else {
            continue;
        };
        let (head, detail) = match reason.split_once(": ") {
            Some((head, detail)) => (head, detail),
            None => (reason.as_str(), ""),
        };
        let entry = groups.entry(head).or_insert((0, Vec::new()));
        entry.0 += 1;
        if !detail.is_empty() && entry.1.len() < 3 {
            entry.1.push(detail);
        }
    }
    if groups.is_empty() {
        return;
    }

    html.push_str("<section>\n<h2>Why work ran</h2>\n");
    html.push_str(
        "<p class=\"note\">The same reasons <code>--explain</code> prints, \
         counted. These are decisions the journal recorded during this build, \
         not a reconstruction.</p>\n",
    );
    html.push_str(
        "<table>\n<thead><tr><th>reason</th><th class=\"num\">actions</th>\
         <th>for example</th></tr></thead>\n<tbody>\n",
    );
    let mut rows: Vec<_> = groups.into_iter().collect();
    rows.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then_with(|| a.0.cmp(b.0)));
    for (head, (count, examples)) in rows {
        let _ = writeln!(
            html,
            "<tr><td>{}</td><td class=\"num\">{count}</td><td class=\"dim\">{}</td></tr>",
            escape(head),
            escape(&examples.join(", ")),
        );
    }
    html.push_str("</tbody>\n</table>\n</section>\n");
}

fn tests(html: &mut String, build: &Build) {
    let tests: Vec<&frostbuild_exec::ActionResult> =
        build.report.results.iter().filter(|r| is_test(r)).collect();
    if tests.is_empty() {
        return;
    }
    let passed = tests
        .iter()
        .filter(|t| matches!(t.outcome, Outcome::Executed { .. }))
        .count();
    let cached = tests
        .iter()
        .filter(|t| matches!(t.outcome, Outcome::Cached))
        .count();
    let failed = tests.len() - passed - cached;

    html.push_str("<section>\n<h2>Tests</h2>\n");
    let _ = writeln!(
        html,
        "<p class=\"note\">{passed} passed, {failed} failed, {cached} cached. \
         A cached test passed on inputs identical to these; it is a result, not \
         a skip.</p>"
    );
    html.push_str(
        "<table>\n<thead><tr><th>test</th><th>shard</th><th class=\"num\">ms</th>\
         <th>outcome</th></tr></thead>\n<tbody>\n",
    );
    for test in tests {
        // `test:<name>` for a whole test, `test:<name>#<i>/<n>` for one shard
        // of a sharded one, which is the only place the index is recorded.
        let bare = test.id.strip_prefix("test:").unwrap_or(&test.id);
        let (name, shard) = match bare.split_once('#') {
            Some((name, shard)) => (name, shard.to_string()),
            None => (bare, "—".to_string()),
        };
        let _ = writeln!(
            html,
            "<tr><td>{}</td><td class=\"dim\">{}</td><td class=\"num\">{}</td>\
             <td>{}</td></tr>",
            escape(name),
            escape(&shard),
            duration_cell(&test.outcome),
            outcome_cell(&test.outcome),
        );
    }
    html.push_str("</tbody>\n</table>\n</section>\n");
}

fn failures(html: &mut String, build: &Build) {
    let failed: Vec<&frostbuild_exec::ActionResult> = build
        .report
        .results
        .iter()
        .filter(|result| matches!(result.outcome, Outcome::Failed { .. }))
        .collect();
    if failed.is_empty() {
        return;
    }

    html.push_str("<section>\n<h2>Failures</h2>\n");
    for result in failed {
        let Outcome::Failed { reason, detail } = &result.outcome else {
            continue;
        };
        let _ = writeln!(
            html,
            "<h3 class=\"fail\">{}</h3>\n<p class=\"note\">{} · {}</p>",
            escape(&result.desc),
            escape(&result.target),
            escape(reason),
        );
        // The end of the output, because that is where a compiler says what
        // went wrong; the start is usually the command line, which is already
        // in the reason.
        let lines: Vec<&str> = detail.lines().collect();
        let skipped = lines.len().saturating_sub(FAILURE_TAIL_LINES);
        if skipped > 0 {
            let _ = writeln!(
                html,
                "<p class=\"note\">last {FAILURE_TAIL_LINES} of {} lines</p>",
                lines.len()
            );
        }
        let _ = writeln!(html, "<pre>{}</pre>", escape(&lines[skipped..].join("\n")));
    }
    html.push_str("</section>\n");
}

fn footer(html: &mut String, destination: &Path, build: &Build) {
    html.push_str("<footer>\n");
    if let Some(trace) = build.trace {
        // The trace is the timeline; this is the summary. Linking rather than
        // embedding keeps the boundary between them visible. The link is
        // relative in both the href and the text, so the pair survives being
        // copied somewhere else together.
        let link = relative_link(destination, trace);
        let _ = writeln!(
            html,
            "<p>Per-action timeline: <a href=\"{0}\">{0}</a>, \
             a Chrome/Perfetto trace to open in <code>chrome://tracing</code> \
             or <code>ui.perfetto.dev</code>.</p>",
            escape(&link),
        );
    }
    html.push_str(
        "<p>Written by <code>frost --report</code>. Every number here comes \
         from the journal, the scheduler and the same decisions \
         <code>--explain</code> prints; the report measures nothing itself.</p>\n",
    );
    html.push_str("</footer>\n");
}

fn tile(html: &mut String, label: &str, value: &str, detail: Option<&str>) {
    let _ = write!(
        html,
        "<div class=\"tile\"><div class=\"label\">{}</div>\
         <div class=\"value\">{}</div>",
        escape(label),
        escape(value)
    );
    if let Some(detail) = detail {
        let _ = write!(html, "<div class=\"detail\">{}</div>", escape(detail));
    }
    html.push_str("</div>\n");
}

fn is_test(result: &frostbuild_exec::ActionResult) -> bool {
    result.kind == ActionKind::Test
}

fn kind_name(kind: ActionKind) -> &'static str {
    match kind {
        ActionKind::Compile => "compile",
        ActionKind::Archive => "archive",
        ActionKind::Link => "link",
        ActionKind::Genrule => "genrule",
        ActionKind::Test => "test",
        ActionKind::KofunCompile => "kofun",
        ActionKind::Command => "command",
    }
}

fn duration_cell(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Executed { duration_ms, .. } => duration_ms.to_string(),
        _ => "—".to_string(),
    }
}

fn outcome_cell(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Executed { .. } => "<span class=\"ran\">ran</span>".to_string(),
        Outcome::Cached => "<span class=\"hit\">cached</span>".to_string(),
        Outcome::Failed { .. } => "<span class=\"fail\">failed</span>".to_string(),
        Outcome::Skipped { reason } => {
            format!("<span class=\"dim\">skipped — {}</span>", escape(reason))
        }
        Outcome::WouldRun { .. } | Outcome::MayRun { .. } => "<span class=\"dim\">—</span>".into(),
    }
}

fn describe_targets(targets: &[String]) -> String {
    match targets {
        [] => "default targets".to_string(),
        [one] => one.clone(),
        many if many.len() <= 4 => many.join(", "),
        many => format!("{} and {} more", many[..3].join(", "), many.len() - 3),
    }
}

/// A link from the report to a sibling file, as a relative path.
///
/// An absolute `file://` URL would break the moment the report is copied
/// somewhere else, which is the thing a single self-contained file is for.
fn relative_link(from_file: &Path, to: &Path) -> String {
    let from_dir = from_file.parent().unwrap_or(Path::new(""));
    let (from, to) = (normalize(from_dir), normalize(to));
    let shared = from
        .iter()
        .zip(to.iter())
        .take_while(|(a, b)| a == b)
        .count();
    // Nothing in common (different drives on Windows, say): the absolute path
    // is the only thing that can be correct.
    if shared == 0 && from.first().is_some_and(|first| first.starts_with('/')) {
        return to.join("/");
    }
    let mut parts: Vec<String> =
        std::iter::repeat_n("..".to_string(), from.len() - shared).collect();
    parts.extend(to[shared..].iter().cloned());
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

fn normalize(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            Component::RootDir => Some("/".to_string()),
            Component::Prefix(prefix) => Some(prefix.as_os_str().to_string_lossy().into_owned()),
            Component::CurDir | Component::ParentDir => None,
        })
        .collect()
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Inline, and the only styling there is. A stylesheet fetched from anywhere
/// would make the report depend on the network to be readable, which is the
/// one thing it must not do.
const STYLE: &str = r#"<style>
:root {
  color-scheme: light dark;
  --bg: #fbfbfd; --fg: #16161a; --dim: #6b6b76; --line: #e3e3e9;
  --card: #ffffff; --ran: #7a4bd0; --hit: #1f7a53; --fail: #c0392b;
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #16161a; --fg: #ececf1; --dim: #9a9aa6; --line: #2c2c34;
    --card: #1e1e24; --ran: #b79cf0; --hit: #5ec99a; --fail: #ff8b7d;
  }
}
* { box-sizing: border-box; }
body {
  margin: 0 auto; padding: 2.5rem 1.5rem 4rem; max-width: 60rem;
  background: var(--bg); color: var(--fg);
  font: 15px/1.55 ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
}
h1 { margin: 0; font-size: 1.6rem; letter-spacing: -0.01em; }
h2 { margin: 2.5rem 0 0.5rem; font-size: 1.05rem; letter-spacing: 0.02em;
     text-transform: uppercase; color: var(--dim); }
h3 { margin: 1.5rem 0 0.25rem; font-size: 1rem; }
p { margin: 0.4rem 0; }
.sub { color: var(--dim); margin-bottom: 1.5rem; }
.note { color: var(--dim); font-size: 0.9rem; max-width: 46rem; }
.dim { color: var(--dim); }
.tiles { display: flex; flex-wrap: wrap; gap: 0.75rem; margin: 1rem 0; }
.tile {
  flex: 1 1 8rem; padding: 0.75rem 0.9rem; border: 1px solid var(--line);
  border-radius: 8px; background: var(--card);
}
.tile .label { font-size: 0.75rem; text-transform: uppercase;
               letter-spacing: 0.04em; color: var(--dim); }
.tile .value { font-size: 1.5rem; font-variant-numeric: tabular-nums;
               letter-spacing: -0.02em; }
.tile .detail { font-size: 0.8rem; color: var(--dim); }
table { width: 100%; border-collapse: collapse; margin: 0.75rem 0; font-size: 0.9rem; }
th, td { text-align: left; padding: 0.35rem 0.6rem 0.35rem 0;
         border-bottom: 1px solid var(--line); vertical-align: top; }
th { font-size: 0.75rem; text-transform: uppercase; letter-spacing: 0.04em;
     color: var(--dim); font-weight: 600; }
td.num, th.num { text-align: right; font-variant-numeric: tabular-nums;
                 padding-right: 0.9rem; white-space: nowrap; }
.ran { color: var(--ran); }
.hit { color: var(--hit); }
.fail { color: var(--fail); }
pre {
  overflow-x: auto; padding: 0.75rem 0.9rem; border: 1px solid var(--line);
  border-radius: 8px; background: var(--card); font-size: 0.82rem;
  white-space: pre-wrap; word-break: break-word;
}
code { font-size: 0.9em; }
footer { margin-top: 3rem; padding-top: 1rem; border-top: 1px solid var(--line);
         color: var(--dim); font-size: 0.85rem; }
a { color: inherit; }
</style>
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use frostbuild_exec::{ActionResult, BuildStats};

    fn action(id: &str, kind: ActionKind, outcome: Outcome) -> ActionResult {
        ActionResult {
            id: id.to_string(),
            desc: format!("DESC {id}"),
            kind,
            target: "//app:app".to_string(),
            outcome,
        }
    }

    fn ran(reason: &str, ms: u64) -> Outcome {
        Outcome::Executed {
            reason: reason.to_string(),
            duration_ms: ms,
        }
    }

    fn build_of(results: Vec<ActionResult>, critical_path: Vec<String>) -> BuildReport {
        BuildReport {
            stats: BuildStats {
                scheduler: "critical-path",
                estimator: "journal",
                jobs: 8,
                makespan_ms: 120,
                busy_ms: 300,
                critical_path_ms: 90,
                estimated_work_ms: 310,
                executed: results
                    .iter()
                    .filter(|r| matches!(r.outcome, Outcome::Executed { .. }))
                    .count(),
            },
            results,
            critical_path,
        }
    }

    fn render_of(report: &BuildReport, test_mode: bool) -> String {
        render(
            Path::new(".frost/report/host-debug.html"),
            &Build {
                workspace: "sample_multi",
                profile: "debug",
                platform: "host",
                targets: &[],
                report,
                graph_actions: 12,
                elapsed_ms: 130,
                trace: None,
                test_mode,
            },
        )
    }

    #[test]
    fn a_report_reaches_nothing_outside_itself() {
        // The whole premise is a file that can be handed to someone and opened.
        // A stylesheet, a script or an image fetched from anywhere makes it a
        // page that needs the network to be readable.
        let report = build_of(
            vec![
                action(
                    "compile:app:src/main.c",
                    ActionKind::Compile,
                    ran("not built before", 40),
                ),
                action("link:app", ActionKind::Link, Outcome::Cached),
            ],
            vec!["compile:app:src/main.c".to_string()],
        );
        let html = render_of(&report, false);

        for reference in [
            "http://",
            "https://",
            "src=\"//",
            "href=\"//",
            "@import",
            "<script",
        ] {
            assert!(
                !html.contains(reference),
                "the report reached outside itself with {reference}"
            );
        }
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("<style>"), "styling has to be inline");
    }

    #[test]
    fn everything_rendered_is_escaped() {
        // Reasons and failure output are tool output: they contain whatever a
        // compiler printed, and a build report is not a place to execute it.
        let report = build_of(
            vec![action(
                "genrule:evil",
                ActionKind::Genrule,
                Outcome::Failed {
                    reason: "exit status 1".to_string(),
                    detail: "<script>alert('x')</script> & \"quoted\"".to_string(),
                },
            )],
            Vec::new(),
        );
        let html = render_of(&report, false);

        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(!html.contains("<script>alert"), "{html}");
        assert!(html.contains("&amp; &quot;quoted&quot;"), "{html}");
    }

    #[test]
    fn a_fully_cached_build_says_so_instead_of_reporting_zeroes() {
        let report = build_of(
            vec![
                action(
                    "compile:app:src/main.c",
                    ActionKind::Compile,
                    Outcome::Cached,
                ),
                action("link:app", ActionKind::Link, Outcome::Cached),
            ],
            Vec::new(),
        );
        let mut report = report;
        report.stats.executed = 0;
        let html = render_of(&report, false);

        assert!(html.contains("Nothing ran"), "{html}");
        assert!(!html.contains("utilization"), "{html}");
        assert!(
            !html.contains("Slowest actions"),
            "there is no slowest action when none ran:\n{html}"
        );
    }

    #[test]
    fn reasons_are_grouped_by_explains_vocabulary_not_by_path() {
        // One row per changed file would be a directory listing. The reason a
        // reader acts on is the head of the string, which is what `--explain`
        // names.
        let report = build_of(
            vec![
                action(
                    "compile:a",
                    ActionKind::Compile,
                    ran("input changed: src/a.c", 10),
                ),
                action(
                    "compile:b",
                    ActionKind::Compile,
                    ran("input changed: src/b.c", 11),
                ),
                action(
                    "compile:c",
                    ActionKind::Compile,
                    ran("input changed: src/c.c", 12),
                ),
                action(
                    "compile:d",
                    ActionKind::Compile,
                    ran("input changed: src/d.c", 13),
                ),
                action("link:app", ActionKind::Link, ran("not built before", 5)),
            ],
            Vec::new(),
        );
        let html = render_of(&report, false);

        let grouped = html
            .split("<h2>Why work ran</h2>")
            .nth(1)
            .expect("the reasons section");
        assert!(
            grouped.contains("<td>input changed</td><td class=\"num\">4</td>"),
            "{grouped}"
        );
        assert!(
            grouped.contains("<td>not built before</td><td class=\"num\">1</td>"),
            "{grouped}"
        );
        // Examples, not an enumeration.
        assert!(grouped.contains("src/a.c, src/b.c, src/c.c"), "{grouped}");
        assert!(!grouped.contains("src/d.c"), "{grouped}");
    }

    #[test]
    fn a_shard_is_reported_as_a_slice_of_its_test() {
        let report = build_of(
            vec![
                action(
                    "test:unit#0/2",
                    ActionKind::Test,
                    ran("not built before", 20),
                ),
                action("test:unit#1/2", ActionKind::Test, Outcome::Cached),
            ],
            Vec::new(),
        );
        let html = render_of(&report, true);

        let tests = html
            .split("<h2>Tests</h2>")
            .nth(1)
            .expect("the tests section");
        assert!(
            tests.contains("<td>unit</td><td class=\"dim\">0/2</td>"),
            "{tests}"
        );
        assert!(
            tests.contains("<td>unit</td><td class=\"dim\">1/2</td>"),
            "{tests}"
        );
        assert!(tests.contains("1 passed, 0 failed, 1 cached"), "{tests}");
    }

    #[test]
    fn a_trace_written_beside_the_report_is_linked_relatively() {
        // An absolute path breaks the moment the report is copied somewhere
        // else, which is what a single self-contained file is for.
        assert_eq!(
            relative_link(
                Path::new(".frost/report/host-debug.html"),
                Path::new("trace.json")
            ),
            "../../trace.json"
        );
        assert_eq!(
            relative_link(Path::new("out/report.html"), Path::new("out/trace.json")),
            "trace.json"
        );
        assert_eq!(
            relative_link(Path::new("report.html"), Path::new("traces/build.json")),
            "traces/build.json"
        );
    }

    #[test]
    fn rendering_a_large_build_is_not_something_a_build_would_notice() {
        // The report is written after the build has been timed and summarized,
        // so it cannot move the number it reports. This bounds the wall-clock
        // cost anyway, because "does not affect the measurement" and "does not
        // make the command feel slower" are different claims.
        let results: Vec<ActionResult> = (0..5000)
            .map(|i| {
                action(
                    &format!("compile:pkg{}:src/file{i}.c", i % 50),
                    ActionKind::Compile,
                    if i % 3 == 0 {
                        ran(&format!("input changed: src/file{i}.h"), i as u64 % 90)
                    } else {
                        Outcome::Cached
                    },
                )
            })
            .collect();
        let critical: Vec<String> = results.iter().take(60).map(|r| r.id.clone()).collect();
        let report = build_of(results, critical);

        let started = std::time::Instant::now();
        let html = render_of(&report, false);
        let elapsed = started.elapsed();

        assert!(!html.is_empty());
        assert!(
            elapsed < std::time::Duration::from_millis(250),
            "rendering 5000 actions took {elapsed:?}"
        );
    }
}
