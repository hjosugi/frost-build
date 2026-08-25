use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::manifest::{
    ActionResources, Manifest, TargetKind, Toolchain, DEFAULT_PROFILE, HOST_PLATFORM,
};

pub type FileId = usize;
pub type ActionId = usize;

pub const OBJ_DIR: &str = ".frost/obj";
pub const LIB_DIR: &str = ".frost/lib";
pub const BIN_DIR: &str = ".frost/bin";
/// Frost-owned stamps recording the contents of declared `output_dirs`.
pub const TREE_STAMP_DIR: &str = ".frost/tree";
/// Raw coverage counters and the tracefiles merged from them.
pub const COVERAGE_DIR: &str = ".frost/coverage";

/// What instruments a compile and a link for coverage.
///
/// One flag rather than `-fprofile-arcs -ftest-coverage`, because gcc and clang
/// both accept it and it is the spelling their documentation uses; which of the
/// two underlying flags belongs on which command is not something a manifest
/// author should have to know.
const COVERAGE_FLAG: &str = "--coverage";

/// How many leading directory components `GCOV_PREFIX` drops.
///
/// All of them. gcc names a counter file after the *object* it belongs to,
/// prefixing the object's absolute path; keeping any of that would put this
/// machine's checkout path inside a declared output, so two machines building
/// identical sources would record different trees. Flattening is only safe
/// because coverage objects are named to be unique across every object linked
/// into a test — see [`object_key`].
const GCOV_PREFIX_STRIP: u32 = 99;

/// How a build-stamp value is referenced from a manifest. The reference — not
/// the value — is what survives graph construction: values arrive once per
/// build, and the graph must stay a pure function of the manifest so that
/// caching it is sound. See [`crate::stamp`].
const STAMP_OPEN: &str = "${stamp.";

/// The interpreter frost runs every genrule and shell test through.
///
/// frost chooses it, so its identity is frost's responsibility in exactly the
/// way the compiler's is: the toolchain fingerprint hashes it alongside the
/// C drivers. Tools the command itself reaches are a different matter — those
/// are undeclared inputs, and no build system can name them for you.
#[cfg(unix)]
pub const SHELL: &str = "/bin/sh";
#[cfg(unix)]
pub const SHELL_ARG: &str = "-c";

#[cfg(windows)]
pub const SHELL: &str = "cmd.exe";
#[cfg(windows)]
pub const SHELL_ARG: &str = "/C";

#[derive(Debug, Serialize, Deserialize)]
pub struct FileNode {
    pub path: String,
    pub producer: Option<ActionId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionKind {
    Compile,
    Archive,
    Link,
    Genrule,
    Test,
    KofunCompile,
    Command,
    /// Collect one test target's coverage counters and emit lcov. Executed by
    /// the engine rather than spawned, because the work is one `gcov` call per
    /// counter file plus a merge, and a shell pipeline that did the same would
    /// need `lcov` or `gcovr` — neither of which ships with a toolchain.
    Coverage,
}

/// What a [`ActionKind::Coverage`] action reads and writes.
///
/// The paths are named rather than discovered by scanning the output tree: an
/// action that globbed would report whatever an earlier configuration left
/// behind, and its key could not see the difference.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoverageSpec {
    /// Directories the instrumented test wrote `.gcda` counters into — one per
    /// shard, since each shard resets its own.
    pub gcda_dirs: Vec<String>,
    /// The `.gcno` notes files gcov needs beside those counters. Every object
    /// linked into the test contributes one, including the ones compiled for a
    /// dependency, because the counters are per object and the running binary
    /// writes all of them.
    pub notes: Vec<String>,
    /// Where the tracefile goes.
    pub output: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ActionNode {
    /// Stable identifier, e.g. `compile:app:src/main.c`. Journal entries are
    /// keyed by this, so it must not depend on hashes or ordering.
    pub id: String,
    /// Short human-readable description, e.g. `CC src/main.c (app)`.
    pub desc: String,
    pub kind: ActionKind,
    pub target: String,
    pub sandbox: bool,
    pub argv: Vec<String>,
    /// Additional direct-argv commands executed after `argv`.
    pub followup_argv: Vec<Vec<String>>,
    /// Intermediate workspace directories removed and recreated before each
    /// execution (including determinism reruns).
    pub clean_dirs: Vec<String>,
    /// Retain prior declared outputs while an incremental command reruns.
    #[serde(default)]
    pub preserve_outputs: bool,
    /// Manifest-declared environment values, applied after Frost's baseline.
    pub env: BTreeMap<String, String>,
    /// Host environment names explicitly requested by this action. Their
    /// current values participate in its action key.
    pub pass_env: Vec<String>,
    pub inputs: Vec<FileId>,
    /// Enforce producer completion without adding content to the action key.
    pub order_only_inputs: Vec<FileId>,
    pub outputs: Vec<FileId>,
    /// Workspace-relative directories this action owns entirely. Every file
    /// under one is recorded as an output, which is how a tool whose output
    /// file names cannot be written down in advance is still cacheable.
    #[serde(default)]
    pub output_dirs: Vec<String>,
    /// Workspace-relative path of the depfile this action writes. Absent when
    /// the dependency report is read from captured output instead.
    pub depfile: Option<String>,
    /// Format of that report; see `depfile::Format`.
    #[serde(default)]
    pub depfile_format: crate::depfile::Format,
    /// Extra attempts a failing test gets before the failure is the verdict.
    ///
    /// Not action-key material on purpose: it says how hard to look for a
    /// verdict, not what the test does, so raising it must not invalidate a
    /// result that already passed. Non-test actions leave it at 0 — a compile
    /// that failed does not deserve another try.
    #[serde(default)]
    pub flaky_retries: u32,
    /// Admission tokens reserved while this action runs. Scheduling metadata,
    /// intentionally excluded from the action key.
    #[serde(default)]
    pub resources: ActionResources,
    /// Stamp keys this action references whose values *are* action-key
    /// material. A new commit rebuilding the binary that embeds its SHA is the
    /// correct answer, not cache thrash.
    #[serde(default)]
    pub stable_stamps: Vec<String>,
    /// Stamp keys this action references whose values are not. Feeding a wall
    /// clock to a key would rebuild the world every second; instead an action
    /// that reads one is re-executed unconditionally, which costs one action
    /// rather than the graph below it.
    #[serde(default)]
    pub volatile_stamps: Vec<String>,
    /// Present only on [`ActionKind::Coverage`], which needs paths an argv
    /// cannot carry — the engine runs it in process.
    // Do not use `skip_serializing_if` here: graph stores use postcard's
    // positional struct encoding, where omitting a trailing `None` would make
    // the next action's first byte look like this Option's discriminant.
    #[serde(default)]
    pub coverage: Option<CoverageSpec>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TargetNode {
    pub name: String,
    pub kind: TargetKind,
    pub deps: Vec<String>,
    pub actions: Vec<ActionId>,
    pub outputs: Vec<FileId>,
    /// Seconds any action of this target may run before it is stopped. Carried
    /// on the target rather than the action because it is a property of the
    /// work the author declared, and deliberately absent from the action key:
    /// the same inputs still produce the same result under a different limit.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// The `[target.*.platform.NAME]` section that shaped this target, when one
    /// did. Recorded here rather than recomputed, so a reader asking why a
    /// cross build looks the way it does gets an answer from the graph it
    /// actually built — including from a cached graph, where the manifest is
    /// never re-read.
    #[serde(default)]
    pub applied_platform: Option<String>,
}

/// A workspace-relative path glob, with the semantics every query surface
/// shares: `*` and `?` stop at `/`, `**` crosses it, and a pattern written with
/// backslashes still matches the `/` form the graph stores.
pub struct PathPattern(glob::Pattern);

impl PathPattern {
    pub fn new(pattern: &str) -> Result<Self> {
        let normalized = pattern.replace('\\', "/");
        Ok(Self(glob::Pattern::new(&normalized).with_context(
            || format!("invalid path pattern {pattern:?}"),
        )?))
    }

    pub fn matches(&self, path: &str) -> bool {
        // Without `require_literal_separator` a `*` swallows `/`, so `src/*.c`
        // would match `src/deep/nested.c` and a caller asking about one
        // directory would silently get another's answer.
        self.0.matches_with(
            path,
            glob::MatchOptions {
                require_literal_separator: true,
                ..Default::default()
            },
        )
    }
}

/// Result of [`BuildGraph::allpaths`].
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AllPaths {
    pub paths: Vec<Vec<String>>,
    /// The walk stopped at its limit and more paths exist. Reported rather
    /// than silently dropped, because a partial answer to "what would I have
    /// to cut" is worse than a slow one only if the reader believes it is
    /// complete.
    pub truncated: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct BuildGraph {
    pub files: Vec<FileNode>,
    pub actions: Vec<ActionNode>,
    pub targets: BTreeMap<String, TargetNode>,
    pub profile: String,
    /// Platform this graph was configured for; `host` uses the root
    /// `[toolchain]` and historical (platform-free) output paths.
    pub platform: String,
    /// Platform-resolved toolchain, embedded so warm invocations can compute
    /// the toolchain fingerprint without re-parsing the manifest.
    pub toolchain: Toolchain,
    /// Workspace default targets, embedded for the same reason.
    pub default_targets: Vec<String>,
    /// The `[stamp]` section, embedded for the same reason again: a warm
    /// invocation has to run the stamp command before it can execute anything
    /// that reads one, and it gets here without having parsed a manifest.
    #[serde(default)]
    pub stamp: Option<crate::manifest::Stamp>,
    /// Whether this graph's C/C++ actions are instrumented for coverage. Part
    /// of the configuration, like `profile` and `platform`, so an instrumented
    /// graph is never mistaken for an ordinary one — see [`crate::paths::configured`].
    #[serde(default)]
    pub coverage: bool,
    #[serde(skip)]
    file_ids: HashMap<String, FileId>,
}

/// Test options supplied on the command line rather than in the manifest.
///
/// These are applied to the loaded graph in memory, never to the stored one:
/// a graph compiled with `--test-filter parse` must not be reused by the next
/// invocation without it.
#[derive(Debug, Default, Clone)]
pub struct TestOptions {
    /// Which cases to run, passed to the runner through the environment.
    pub filter: Option<String>,
    /// Extra environment for every test action.
    pub env: Vec<(String, String)>,
    /// Extra arguments appended to every test's argv.
    pub args: Vec<String>,
}

impl TestOptions {
    pub fn is_empty(&self) -> bool {
        self.filter.is_none() && self.env.is_empty() && self.args.is_empty()
    }
}

/// The variable Bazel-compatible runners read to learn which cases to run, and
/// googletest's own spelling of it — the same pair sharding already passes, for
/// the same reason: Frost cannot know a runner's filter flag, and guessing one
/// per language is how a build tool acquires a table of special cases.
pub const TEST_FILTER_VARS: [&str; 2] = ["TESTBRIDGE_TEST_ONLY", "GTEST_FILTER"];

impl BuildGraph {
    /// Fold command-line test options into every test action.
    ///
    /// Nothing new has to enter the action key for these to be safe: argv and
    /// env are already key material, so a filtered run keys differently from an
    /// unfiltered one and cannot satisfy it from cache. That is the behaviour
    /// #142 asked for, and it falls out rather than being bolted on.
    ///
    /// The command line wins over a manifest value of the same name. It is the
    /// person typing now, and because the override lands in the key it changes
    /// the result visibly instead of silently.
    pub fn apply_test_options(&mut self, options: &TestOptions) {
        if options.is_empty() {
            return;
        }
        for action in &mut self.actions {
            if action.kind != ActionKind::Test {
                continue;
            }
            action.argv.extend(options.args.iter().cloned());
            if let Some(filter) = &options.filter {
                for name in TEST_FILTER_VARS {
                    action.env.insert(name.to_string(), filter.clone());
                }
            }
            for (key, value) in &options.env {
                action.env.insert(key.clone(), value.clone());
            }
            // A name given a value here is no longer inherited from the host,
            // and leaving it in `pass_env` would put the host's value in the
            // key beside the one that actually applies.
            action
                .pass_env
                .retain(|name| !action.env.contains_key(name));
        }
    }
}

impl BuildGraph {
    pub fn from_manifest(manifest: &Manifest) -> Result<Self> {
        Self::from_manifest_with_profile(manifest, "debug")
    }

    pub fn from_manifest_with_profile(manifest: &Manifest, profile: &str) -> Result<Self> {
        Self::from_manifest_configured(manifest, profile, HOST_PLATFORM)
    }

    pub fn from_manifest_configured(
        manifest: &Manifest,
        profile: &str,
        platform: &str,
    ) -> Result<Self> {
        Self::from_manifest_instrumented(manifest, profile, platform, false)
    }

    /// The configured graph, optionally instrumented for coverage.
    ///
    /// Coverage is threaded through as a flag rather than as a profile because
    /// it has to reach the output tree without being a name the manifest
    /// declares; [`crate::paths::configured`] holds that reasoning.
    pub fn from_manifest_instrumented(
        manifest: &Manifest,
        profile: &str,
        platform: &str,
        coverage: bool,
    ) -> Result<Self> {
        if profile.is_empty()
            || !profile
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            bail!("invalid profile name {profile:?}");
        }
        // A profile the manifest never declares builds with no profile flags,
        // into its own output tree — silently, so `--profile relase` produces
        // a different binary than `--profile release` and says nothing. Once a
        // workspace declares any profile, an undeclared name is a typo far
        // more often than an intent; declaring an empty section is the way to
        // ask for a bare tree on purpose.
        if !manifest.profiles.is_empty()
            && profile != DEFAULT_PROFILE
            && !manifest.profiles.contains_key(profile)
        {
            let known: Vec<&str> = manifest.profiles.keys().map(String::as_str).collect();
            if let Some(hint) = crate::manifest::closest(profile, known.iter().copied()) {
                bail!("unknown profile {profile:?}. did you mean {hint:?}?");
            }
            bail!(
                "unknown profile {profile:?}. declared profiles: {} \
                 (add an empty [profile.{profile}] section for a bare tree)",
                known.join(", ")
            );
        }
        let toolchain = manifest.toolchain_for(platform)?;
        let tree = crate::paths::configured(platform, profile, coverage);
        let order = toposort_targets(manifest)?;
        let mut graph = BuildGraph {
            profile: profile.to_string(),
            platform: platform.to_string(),
            toolchain: toolchain.clone(),
            default_targets: manifest.default_targets.clone(),
            stamp: manifest.stamp.clone(),
            coverage,
            ..BuildGraph::default()
        };
        let profile_flags = manifest.profiles.get(profile).cloned().unwrap_or_default();

        // Transitive exported include dirs, library outputs and genrule
        // outputs, per target — held as structurally shared sets so deep
        // dependency chains stay O(targets + edges) to propagate. Flattening
        // happens only where a flat list is genuinely needed (compile -I
        // flags, order-only generated inputs, link lines) — see #78.
        let mut exported_includes: HashMap<String, Rc<SharedSet>> = HashMap::new();
        let mut exported_libs: HashMap<String, Rc<SharedSet>> = HashMap::new();
        let mut genrule_outputs: HashMap<String, Rc<SharedSet>> = HashMap::new();
        // `.gcno` notes per C/C++ target, own plus inherited. Only C/C++
        // targets appear, so a lookup is `get` rather than an index: a genrule
        // dependency contributes no objects and therefore no counters.
        let mut coverage_notes: HashMap<String, Vec<String>> = HashMap::new();

        for name in &order {
            // The overlay is applied once, here, so everything downstream sees
            // one target with one set of values. A conditional that survived
            // into action construction would have to be re-evaluated at every
            // use, and the first place someone forgot would be a build that
            // differs from what the manifest says.
            let resolved = manifest.targets[name].for_platform(platform);
            let applied_platform =
                matches!(resolved, std::borrow::Cow::Owned(_)).then(|| platform.to_string());
            let target = resolved.as_ref();

            let dep_sets = |map: &HashMap<String, Rc<SharedSet>>| -> Vec<Rc<SharedSet>> {
                target.deps.iter().map(|dep| map[dep].clone()).collect()
            };
            let include_set =
                SharedSet::join(target.includes.clone(), dep_sets(&exported_includes));
            let lib_parents = dep_sets(&exported_libs);
            let gen_parents = dep_sets(&genrule_outputs);

            let mut target_node = TargetNode {
                name: name.clone(),
                kind: target.kind,
                deps: target.deps.clone(),
                actions: Vec::new(),
                outputs: Vec::new(),
                timeout_secs: target.timeout_secs,
                applied_platform,
            };

            match target.kind {
                TargetKind::Genrule => {
                    let cmd = target.cmd.as_deref().unwrap();
                    let mut inputs = Vec::new();
                    for p in &target.inputs {
                        inputs.push(graph.file(p));
                    }
                    // Order after dep targets by consuming their outputs, and
                    // keep them keyed by label so the cmd can name one without
                    // writing out that dependency's layout convention.
                    let mut dependency_map: Vec<(String, Vec<String>)> = Vec::new();
                    for dep in &target.deps {
                        for out in dep_outputs(&graph, dep) {
                            inputs.push(out);
                        }
                        dependency_map.push((dep.clone(), dep_output_paths(&graph, dep)));
                    }
                    let expanded = expand_genrule_cmd(
                        cmd,
                        &target.inputs,
                        &target.outputs,
                        &dependency_map,
                        name,
                    )?;
                    let mut outputs = Vec::new();
                    for p in &target.outputs {
                        outputs.push(graph.file(p));
                    }
                    let action = graph.push_action(ActionNode {
                        id: format!("genrule:{name}"),
                        desc: format!("GEN {name}"),
                        kind: ActionKind::Genrule,
                        target: name.clone(),
                        sandbox: target.sandbox,
                        argv: vec![SHELL.into(), SHELL_ARG.into(), expanded],
                        followup_argv: Vec::new(),
                        clean_dirs: Vec::new(),
                        preserve_outputs: false,
                        env: BTreeMap::new(),
                        pass_env: Vec::new(),
                        inputs,
                        order_only_inputs: Vec::new(),
                        outputs: outputs.clone(),
                        output_dirs: Vec::new(),
                        depfile: None,
                        depfile_format: crate::depfile::Format::Make,
                        flaky_retries: 0,
                        resources: target.resources,
                        stable_stamps: Vec::new(),
                        volatile_stamps: Vec::new(),
                        coverage: None,
                    })?;
                    target_node.actions.push(action);
                    target_node.outputs = outputs;
                    genrule_outputs.insert(
                        name.clone(),
                        SharedSet::join(target.outputs.clone(), gen_parents),
                    );
                    exported_libs.insert(name.clone(), SharedSet::join(Vec::new(), lib_parents));
                    exported_includes.insert(name.clone(), include_set);
                }
                TargetKind::Test => {
                    let mut inputs = target
                        .inputs
                        .iter()
                        .map(|p| graph.file(p))
                        .collect::<Vec<_>>();
                    for dep in &target.deps {
                        inputs.extend(dep_outputs(&graph, dep));
                    }
                    let (argv, followup_argv, env, pass_env) =
                        if let Some(tool_name) = target.tool.as_deref() {
                            let Some(driver) = toolchain.tools.get(tool_name) else {
                                let configured = toolchain
                                    .tools
                                    .keys()
                                    .map(String::as_str)
                                    .collect::<Vec<_>>();
                                bail!(
                                "test {name:?} uses tool {tool_name:?}, but the active platform \
                                     does not configure [toolchain.tools].{tool_name}{}",
                                if configured.is_empty() {
                                    String::new()
                                } else {
                                    format!(" (configured: {})", configured.join(", "))
                                }
                            );
                            };
                            if driver.contains('/') && !Path::new(driver).is_absolute() {
                                let tool_input = graph.file(driver);
                                if !inputs.contains(&tool_input) {
                                    inputs.push(tool_input);
                                }
                            }
                            let dependency_paths = target
                                .deps
                                .iter()
                                .flat_map(|dep| dep_outputs(&graph, dep))
                                .map(|file| graph.files[file].path.clone())
                                .collect::<Vec<_>>();
                            let dependency_map = target
                                .deps
                                .iter()
                                .map(|dep| (dep.clone(), dep_output_paths(&graph, dep)))
                                .collect::<Vec<_>>();
                            let argv = expand_test_args(
                                driver,
                                &target.args,
                                &target.inputs,
                                &dependency_paths,
                                &dependency_map,
                                &tree,
                                profile,
                                platform,
                            )?;
                            (
                                argv,
                                Vec::new(),
                                expand_env_dep_refs(&target.env, &dependency_map, name)?,
                                target.pass_env.clone(),
                            )
                        } else {
                            (
                                vec![
                                    SHELL.into(),
                                    SHELL_ARG.into(),
                                    target
                                        .cmd
                                        .as_deref()
                                        .expect("test validation requires cmd or tool")
                                        .into(),
                                ],
                                Vec::new(),
                                BTreeMap::new(),
                                Vec::new(),
                            )
                        };
                    // Each shard is a whole action: its own key, its own cache
                    // entry, its own place in the schedule. One shard failing
                    // or being invalidated leaves the others alone.
                    let mut stamp_ids = Vec::new();
                    for shard in test_shards(&tree, name, target.shard_count) {
                        let stamp_id = graph.file(&shard.stamp);
                        let action = graph.push_action(ActionNode {
                            id: shard.id,
                            desc: shard.desc,
                            kind: ActionKind::Test,
                            target: name.clone(),
                            sandbox: target.sandbox,
                            argv: argv.clone(),
                            followup_argv: followup_argv.clone(),
                            clean_dirs: Vec::new(),
                            preserve_outputs: false,
                            env: merge_shard_env(&env, &pass_env, &shard.env, name)?,
                            pass_env: pass_env.clone(),
                            inputs: inputs.clone(),
                            order_only_inputs: Vec::new(),
                            outputs: vec![stamp_id],
                            output_dirs: Vec::new(),
                            depfile: None,
                            depfile_format: crate::depfile::Format::Make,
                            flaky_retries: target.flaky_retries,
                            resources: target.resources,
                            stable_stamps: Vec::new(),
                            volatile_stamps: Vec::new(),
                            coverage: None,
                        })?;
                        target_node.actions.push(action);
                        stamp_ids.push(stamp_id);
                    }
                    target_node.outputs = stamp_ids;
                    exported_libs.insert(name.clone(), SharedSet::join(Vec::new(), lib_parents));
                    exported_includes.insert(name.clone(), include_set);
                    genrule_outputs.insert(name.clone(), SharedSet::join(Vec::new(), gen_parents));
                }
                TargetKind::Command => {
                    let tool_name = target
                        .tool
                        .as_deref()
                        .expect("command target validation requires a tool");
                    let Some(driver) = toolchain.tools.get(tool_name) else {
                        let configured = toolchain
                            .tools
                            .keys()
                            .map(String::as_str)
                            .collect::<Vec<_>>();
                        bail!(
                            "target {name:?} uses tool {tool_name:?}, but the active platform \
                             does not configure [toolchain.tools].{tool_name}{}",
                            if configured.is_empty() {
                                String::new()
                            } else {
                                format!(" (configured: {})", configured.join(", "))
                            }
                        );
                    };
                    let mut inputs = target
                        .inputs
                        .iter()
                        .map(|path| graph.file(path))
                        .collect::<Vec<_>>();
                    if driver.contains('/') && !Path::new(driver).is_absolute() {
                        let tool_input = graph.file(driver);
                        if !inputs.contains(&tool_input) {
                            inputs.push(tool_input);
                        }
                    }
                    let mut dependency_inputs = Vec::new();
                    // Keyed by label as well as flattened, so `${dep:LABEL}`
                    // can name one dependency's output without the manifest
                    // repeating that dependency's layout convention.
                    let mut dependency_map: Vec<(String, Vec<String>)> = Vec::new();
                    for dep in &target.deps {
                        for output in dep_outputs(&graph, dep) {
                            if !inputs.contains(&output) {
                                inputs.push(output);
                            }
                            dependency_inputs.push(output);
                        }
                        dependency_map.push((dep.clone(), dep_output_paths(&graph, dep)));
                    }
                    let input_paths = target.inputs.clone();
                    let dependency_paths = dependency_inputs
                        .iter()
                        .map(|&file| graph.files[file].path.clone())
                        .collect::<Vec<_>>();
                    let outputs = target
                        .outputs
                        .iter()
                        .map(|path| expand_config_template(path, &tree, profile, platform, false))
                        .collect::<Result<Vec<_>>>()?;
                    let depfile = target
                        .depfile
                        .as_ref()
                        .map(|path| expand_config_template(path, &tree, profile, platform, false))
                        .transpose()?;
                    let output_dirs = target
                        .output_dirs
                        .iter()
                        .map(|path| expand_config_template(path, &tree, profile, platform, false))
                        .collect::<Result<Vec<_>>>()?;
                    let clean_dirs = target
                        .clean_dirs
                        .iter()
                        .map(|path| expand_config_template(path, &tree, profile, platform, false))
                        .collect::<Result<Vec<_>>>()?;
                    let argv = expand_command_args(
                        driver,
                        &target.args,
                        &input_paths,
                        &dependency_paths,
                        &dependency_map,
                        &outputs,
                        &output_dirs,
                        &clean_dirs,
                        depfile.as_deref(),
                        &tree,
                        profile,
                        platform,
                    )?;
                    let mut followup_argv = Vec::with_capacity(target.steps.len());
                    for step in &target.steps {
                        let Some(step_driver) = toolchain.tools.get(&step.tool) else {
                            bail!(
                                "target {name:?} uses step tool {:?}, but the active platform \
                                 does not configure [toolchain.tools].{}",
                                step.tool,
                                step.tool
                            );
                        };
                        if step_driver.contains('/') && !Path::new(step_driver).is_absolute() {
                            let tool_input = graph.file(step_driver);
                            if !inputs.contains(&tool_input) {
                                inputs.push(tool_input);
                            }
                        }
                        followup_argv.push(expand_command_args(
                            step_driver,
                            &step.args,
                            &input_paths,
                            &dependency_paths,
                            &dependency_map,
                            &outputs,
                            &output_dirs,
                            &clean_dirs,
                            depfile.as_deref(),
                            &tree,
                            profile,
                            platform,
                        )?);
                    }
                    // An owned directory is not a graph file, so dependents
                    // would have nothing to wait for and no content to notice
                    // changing. Frost writes a stamp naming every file in the
                    // recorded tree with its digest: dependents take an edge to
                    // it, and an identical tree produces an identical stamp, so
                    // early cutoff works on trees exactly as on single files.
                    let mut action_outputs = outputs.clone();
                    if !output_dirs.is_empty() {
                        action_outputs.push(format!(
                            "{TREE_STAMP_DIR}/{tree}/{}/contents",
                            path_key(name)
                        ));
                    }
                    let output_ids = action_outputs
                        .iter()
                        .map(|path| graph.file(path))
                        .collect::<Vec<_>>();
                    let env = expand_env_dep_refs(&target.env, &dependency_map, name)?;
                    let (stable_stamps, volatile_stamps) = collect_stamps(
                        std::iter::once(&argv)
                            .chain(followup_argv.iter())
                            .flat_map(|command| command.iter())
                            .chain(env.values()),
                        manifest.stamp.as_ref(),
                        name,
                    )?;
                    let action = graph.push_action(ActionNode {
                        id: format!("command:{name}"),
                        desc: format!("RUN {name} [{tool_name}]"),
                        kind: ActionKind::Command,
                        target: name.clone(),
                        sandbox: target.sandbox,
                        argv,
                        followup_argv,
                        clean_dirs,
                        preserve_outputs: target.preserve_outputs,
                        env,
                        pass_env: target.pass_env.clone(),
                        inputs,
                        order_only_inputs: Vec::new(),
                        outputs: output_ids.clone(),
                        output_dirs,
                        depfile,
                        depfile_format: target.depfile_format,
                        flaky_retries: 0,
                        resources: target.resources,
                        stable_stamps,
                        volatile_stamps,
                        coverage: None,
                    })?;
                    target_node.actions.push(action);
                    target_node.outputs = output_ids;
                    exported_libs.insert(name.clone(), SharedSet::join(Vec::new(), lib_parents));
                    exported_includes.insert(name.clone(), include_set);
                    genrule_outputs.insert(name.clone(), SharedSet::join(outputs, gen_parents));
                }
                TargetKind::KofunBinary => {
                    let Some(driver) = toolchain.kofunc.as_ref() else {
                        bail!(
                            "target {name:?} is a kofun_binary but [toolchain] \
                             does not configure kofunc"
                        );
                    };
                    if target.srcs.len() != 1 {
                        bail!(
                            "target {name:?} is a kofun_binary with {} expanded sources; \
                             exactly one is required",
                            target.srcs.len()
                        );
                    }

                    let source = &target.srcs[0];
                    let bin = binary_path(&tree, name);
                    let emitted_c = format!("{OBJ_DIR}/{tree}/{}/kofun.c", path_key(name));
                    let mut inputs = vec![graph.file(source)];
                    for dep in &target.deps {
                        for output in dep_outputs(&graph, dep) {
                            if !inputs.contains(&output) {
                                inputs.push(output);
                            }
                        }
                    }
                    let bin_id = graph.file(&bin);
                    let emitted_c_id = graph.file(&emitted_c);
                    let action = graph.push_action(ActionNode {
                        id: format!("kofun:{name}"),
                        desc: format!("KOFUN {source} ({name})"),
                        kind: ActionKind::KofunCompile,
                        target: name.clone(),
                        sandbox: target.sandbox,
                        argv: vec![
                            driver.clone(),
                            "build".into(),
                            source.clone(),
                            "-o".into(),
                            bin.clone(),
                            "--emit-c".into(),
                            emitted_c,
                        ],
                        followup_argv: Vec::new(),
                        clean_dirs: Vec::new(),
                        preserve_outputs: false,
                        env: BTreeMap::new(),
                        pass_env: Vec::new(),
                        inputs,
                        order_only_inputs: Vec::new(),
                        outputs: vec![bin_id, emitted_c_id],
                        output_dirs: Vec::new(),
                        depfile: None,
                        depfile_format: crate::depfile::Format::Make,
                        flaky_retries: 0,
                        resources: target.resources,
                        stable_stamps: Vec::new(),
                        volatile_stamps: Vec::new(),
                        coverage: None,
                    })?;
                    target_node.actions.push(action);
                    target_node.outputs = vec![bin_id];
                    exported_libs.insert(name.clone(), SharedSet::join(Vec::new(), lib_parents));
                    exported_includes.insert(name.clone(), include_set);
                    genrule_outputs.insert(name.clone(), SharedSet::join(Vec::new(), gen_parents));
                }
                TargetKind::CcBinary | TargetKind::CcLibrary | TargetKind::CcTest => {
                    let tc = &toolchain;
                    let own_includes = include_set.flatten();
                    let libs = SharedSet::join(Vec::new(), lib_parents.clone()).flatten();
                    let gen_outs = SharedSet::join(Vec::new(), gen_parents.clone()).flatten();
                    let mut cflags: Vec<String> = tc.cflags.clone();
                    cflags.extend(profile_flags.cflags.iter().cloned());
                    cflags.extend(target.cflags.iter().cloned());
                    let mut include_flags = Vec::new();
                    for dir in &own_includes {
                        include_flags.push(format!("-I{dir}"));
                    }

                    // One compile action per translation unit.
                    let mut objs: Vec<String> = Vec::new();
                    let mut obj_ids: Vec<FileId> = Vec::new();
                    let mut notes: Vec<String> = Vec::new();
                    for src in &target.srcs {
                        let is_cxx = is_cxx_source(src);
                        let driver = if is_cxx { &tc.cxx } else { &tc.cc };
                        let obj = format!(
                            "{OBJ_DIR}/{tree}/{}/{}.o",
                            path_key(name),
                            object_key(name, src, coverage)
                        );
                        let depfile = format!("{obj}.d");
                        let mut argv = vec![driver.clone()];
                        argv.extend(cflags.iter().cloned());
                        if is_cxx {
                            argv.extend(tc.cxxflags.iter().cloned());
                            argv.extend(profile_flags.cxxflags.iter().cloned());
                        }
                        argv.extend(include_flags.iter().cloned());
                        if coverage {
                            argv.push(COVERAGE_FLAG.into());
                        }
                        argv.extend([
                            "-MD".into(),
                            "-MF".into(),
                            depfile.clone(),
                            "-c".into(),
                            src.clone(),
                            "-o".into(),
                            obj.clone(),
                        ]);
                        let inputs = vec![graph.file(src)];
                        // Generated headers from (transitive) genrule deps
                        // must exist before we compile; the depfile narrows
                        // this to the actually-used set on later builds.
                        let order_only_inputs =
                            gen_outs.iter().map(|gen| graph.file(gen)).collect();
                        let obj_id = graph.file(&obj);
                        // The notes file is a real product of the compile, not
                        // a side effect to be rediscovered later: declaring it
                        // puts the raw coverage data in the CAS with everything
                        // else, and means a cache hit restores the pair gcov
                        // needs rather than half of it.
                        let mut outputs = vec![obj_id];
                        if coverage {
                            let note = notes_path(&obj);
                            outputs.push(graph.file(&note));
                            notes.push(note);
                        }
                        let action = graph.push_action(ActionNode {
                            id: format!("compile:{name}:{src}"),
                            desc: format!("CC {src} ({name})"),
                            kind: ActionKind::Compile,
                            target: name.clone(),
                            sandbox: target.sandbox,
                            argv,
                            followup_argv: Vec::new(),
                            clean_dirs: Vec::new(),
                            preserve_outputs: false,
                            env: BTreeMap::new(),
                            pass_env: Vec::new(),
                            inputs,
                            order_only_inputs,
                            outputs,
                            output_dirs: Vec::new(),
                            depfile: Some(depfile),
                            depfile_format: crate::depfile::Format::Make,
                            flaky_retries: 0,
                            resources: target.resources,
                            stable_stamps: Vec::new(),
                            volatile_stamps: Vec::new(),
                            coverage: None,
                        })?;
                        target_node.actions.push(action);
                        objs.push(obj);
                        obj_ids.push(obj_id);
                    }
                    if coverage {
                        // Inherited before use: the counters a test writes
                        // cover every object linked into it, including a
                        // library's, so the notes have to travel with the
                        // dependency edge the same way the archive does.
                        for dep in &target.deps {
                            if let Some(inherited) = coverage_notes.get(dep) {
                                notes.extend(inherited.iter().cloned());
                            }
                        }
                        notes.sort();
                        notes.dedup();
                        coverage_notes.insert(name.clone(), notes.clone());
                    }

                    match target.kind {
                        TargetKind::CcLibrary => {
                            let lib = format!("{LIB_DIR}/{tree}/lib{}.a", path_key(name));
                            let mut argv = vec![tc.ar.clone()];
                            argv.extend(tc.arflags.iter().cloned());
                            argv.push(lib.clone());
                            argv.extend(objs.iter().cloned());
                            let lib_id = graph.file(&lib);
                            let action = graph.push_action(ActionNode {
                                id: format!("archive:{name}"),
                                desc: format!("AR lib{name}.a"),
                                kind: ActionKind::Archive,
                                target: name.clone(),
                                sandbox: target.sandbox,
                                argv,
                                followup_argv: Vec::new(),
                                clean_dirs: Vec::new(),
                                preserve_outputs: false,
                                env: BTreeMap::new(),
                                pass_env: Vec::new(),
                                inputs: obj_ids.clone(),
                                order_only_inputs: Vec::new(),
                                outputs: vec![lib_id],
                                output_dirs: Vec::new(),
                                depfile: None,
                                depfile_format: crate::depfile::Format::Make,
                                flaky_retries: 0,
                                resources: target.resources,
                                stable_stamps: Vec::new(),
                                volatile_stamps: Vec::new(),
                                coverage: None,
                            })?;
                            target_node.actions.push(action);
                            target_node.outputs = vec![lib_id];
                            exported_libs.insert(
                                name.clone(),
                                SharedSet::join(vec![lib.clone()], lib_parents.clone()),
                            );
                        }
                        TargetKind::CcBinary | TargetKind::CcTest => {
                            let bin = binary_path(&tree, name);
                            let link_driver = if target.srcs.iter().any(|s| is_cxx_source(s)) {
                                &tc.cxx
                            } else {
                                &tc.cc
                            };
                            let mut argv = vec![link_driver.clone()];
                            argv.extend(objs.iter().cloned());
                            argv.extend(libs.iter().cloned());
                            argv.extend(tc.ldflags.iter().cloned());
                            argv.extend(profile_flags.ldflags.iter().cloned());
                            argv.extend(target.ldflags.iter().cloned());
                            // The link needs it too: without libgcov the
                            // instrumented objects have nothing to write their
                            // counters with, and the failure is an undefined
                            // `__gcov_*` symbol rather than an empty report.
                            if coverage {
                                argv.push(COVERAGE_FLAG.into());
                            }
                            argv.extend(["-o".into(), bin.clone()]);
                            let mut inputs = obj_ids.clone();
                            for lib in &libs {
                                inputs.push(graph.file(lib));
                            }
                            let bin_id = graph.file(&bin);
                            let action = graph.push_action(ActionNode {
                                id: format!("link:{name}"),
                                desc: format!("LINK {name}"),
                                kind: ActionKind::Link,
                                target: name.clone(),
                                sandbox: target.sandbox,
                                argv,
                                followup_argv: Vec::new(),
                                clean_dirs: Vec::new(),
                                preserve_outputs: false,
                                env: BTreeMap::new(),
                                pass_env: Vec::new(),
                                inputs,
                                order_only_inputs: Vec::new(),
                                outputs: vec![bin_id],
                                output_dirs: Vec::new(),
                                depfile: None,
                                depfile_format: crate::depfile::Format::Make,
                                flaky_retries: 0,
                                resources: target.resources,
                                stable_stamps: Vec::new(),
                                volatile_stamps: Vec::new(),
                                coverage: None,
                            })?;
                            target_node.actions.push(action);
                            target_node.outputs = vec![bin_id];
                            exported_libs.insert(
                                name.clone(),
                                SharedSet::join(Vec::new(), lib_parents.clone()),
                            );
                            if target.kind == TargetKind::CcTest {
                                let mut stamp_ids = Vec::new();
                                let mut counter_stamp_ids = Vec::new();
                                let mut gcda_dirs = Vec::new();
                                for shard in test_shards(&tree, name, target.shard_count) {
                                    let stamp_id = graph.file(&shard.stamp);
                                    let mut outputs = vec![stamp_id];
                                    let mut env = shard.env;
                                    let mut clean_dirs = Vec::new();
                                    let mut output_dirs = Vec::new();
                                    if coverage {
                                        // Two things happen here, and the first
                                        // is what makes the tracefile
                                        // reproducible. `.gcda` counters
                                        // *accumulate*: run the same binary
                                        // twice and the hit counts double, so a
                                        // rerun of an unchanged build would
                                        // report different numbers and read as
                                        // nondeterminism in the build rather
                                        // than in gcov's data model. A
                                        // `clean_dir` is emptied before every
                                        // execution including a
                                        // `--check-determinism` rerun, so the
                                        // property holds by construction.
                                        //
                                        // The second is that they land there at
                                        // all. gcc writes `.gcda` beside the
                                        // object, and the object tree holds
                                        // declared outputs -- it cannot be
                                        // emptied. `GCOV_PREFIX` relocates
                                        // them; `_STRIP` drops the absolute
                                        // path gcc would otherwise mirror,
                                        // whose components differ per machine.
                                        env.insert("GCOV_PREFIX".to_string(), shard.gcda.clone());
                                        env.insert(
                                            "GCOV_PREFIX_STRIP".to_string(),
                                            GCOV_PREFIX_STRIP.to_string(),
                                        );
                                        clean_dirs.push(shard.gcda.clone());
                                        // Raw data is a declared product, so it
                                        // is content-addressed like any other
                                        // and a cached test still has counters
                                        // to report from.
                                        output_dirs.push(shard.gcda.clone());
                                        gcda_dirs.push(shard.gcda.clone());
                                        // The directory's file list and
                                        // digests become a graph input through
                                        // this stamp. A success stamp is empty
                                        // by design and cannot distinguish two
                                        // different counter sets.
                                        let counter_stamp = graph.file(&shard.coverage_stamp);
                                        outputs.push(counter_stamp);
                                        counter_stamp_ids.push(counter_stamp);
                                    }
                                    let test = graph.push_action(ActionNode {
                                        id: shard.id,
                                        desc: shard.desc,
                                        kind: ActionKind::Test,
                                        target: name.clone(),
                                        sandbox: target.sandbox,
                                        argv: vec![bin.clone()],
                                        followup_argv: Vec::new(),
                                        clean_dirs,
                                        preserve_outputs: false,
                                        env,
                                        pass_env: Vec::new(),
                                        inputs: vec![bin_id],
                                        order_only_inputs: Vec::new(),
                                        outputs,
                                        output_dirs,
                                        depfile: None,
                                        depfile_format: crate::depfile::Format::Make,
                                        flaky_retries: target.flaky_retries,
                                        resources: target.resources,
                                        stable_stamps: Vec::new(),
                                        volatile_stamps: Vec::new(),
                                        coverage: None,
                                    })?;
                                    target_node.actions.push(test);
                                    stamp_ids.push(stamp_id);
                                }
                                if coverage {
                                    // One merge per test target rather than one
                                    // for the workspace: a single global merge
                                    // would rerun whenever any test did, and
                                    // "change one file, re-measure one test" is
                                    // the behaviour that makes coverage usable
                                    // in an edit loop.
                                    let notes =
                                        coverage_notes.get(name).cloned().unwrap_or_default();
                                    let lcov =
                                        format!("{COVERAGE_DIR}/{tree}/{}.lcov", path_key(name));
                                    // Content stamps, rather than the empty
                                    // success stamps, put the exact raw
                                    // counter trees in the merge key.
                                    let mut inputs = counter_stamp_ids;
                                    inputs.push(bin_id);
                                    inputs.extend(notes.iter().map(|note| graph.file(note)));
                                    let lcov_id = graph.file(&lcov);
                                    let merge = graph.push_action(ActionNode {
                                        id: format!("coverage:{name}"),
                                        desc: format!("COV {name}"),
                                        kind: ActionKind::Coverage,
                                        target: name.clone(),
                                        sandbox: false,
                                        // The reporter, and only the reporter:
                                        // its identity belongs in the action
                                        // key, and a different `gcov` reads the
                                        // same counters differently.
                                        argv: vec![tc.gcov.clone()],
                                        followup_argv: Vec::new(),
                                        clean_dirs: Vec::new(),
                                        preserve_outputs: false,
                                        env: BTreeMap::new(),
                                        pass_env: Vec::new(),
                                        inputs,
                                        order_only_inputs: Vec::new(),
                                        outputs: vec![lcov_id],
                                        output_dirs: Vec::new(),
                                        depfile: None,
                                        depfile_format: crate::depfile::Format::Make,
                                        flaky_retries: 0,
                                        resources: target.resources,
                                        stable_stamps: Vec::new(),
                                        volatile_stamps: Vec::new(),
                                        coverage: Some(CoverageSpec {
                                            gcda_dirs,
                                            notes,
                                            output: lcov,
                                        }),
                                    })?;
                                    target_node.actions.push(merge);
                                    stamp_ids.push(lcov_id);
                                }
                                target_node.outputs = stamp_ids;
                            }
                        }
                        TargetKind::Genrule
                        | TargetKind::Test
                        | TargetKind::KofunBinary
                        | TargetKind::Command => {
                            unreachable!()
                        }
                    }

                    exported_includes.insert(name.clone(), include_set);
                    genrule_outputs.insert(name.clone(), SharedSet::join(Vec::new(), gen_parents));
                }
            }

            graph.targets.insert(name.clone(), target_node);
        }

        graph.validate_clean_dirs()?;
        graph.validate_volatile_stamps()?;
        Ok(graph)
    }

    fn file(&mut self, path: &str) -> FileId {
        if let Some(&id) = self.file_ids.get(path) {
            return id;
        }
        let id = self.files.len();
        self.files.push(FileNode {
            path: path.to_string(),
            producer: None,
        });
        self.file_ids.insert(path.to_string(), id);
        id
    }

    fn push_action(&mut self, action: ActionNode) -> Result<ActionId> {
        if (action.kind == ActionKind::Coverage) != action.coverage.is_some() {
            bail!(
                "action {:?} must carry coverage metadata exactly when its kind is coverage",
                action.id
            );
        }
        let id = self.actions.len();
        for &out in &action.outputs {
            if let Some(other) = self.files[out].producer {
                bail!(
                    "output {:?} is produced by both {:?} and {:?}",
                    self.files[out].path,
                    self.actions[other].id,
                    action.id
                );
            }
            self.files[out].producer = Some(id);
        }
        self.actions.push(action);
        Ok(id)
    }

    /// A volatile stamp value must not reach a compile.
    ///
    /// This is the failure the whole stable/volatile split exists to prevent,
    /// and it is not obvious from the manifest that wrote it. A command target
    /// that writes `version.h` containing a build timestamp re-runs every
    /// build by design — that part is cheap and intended. But the header's
    /// bytes then differ every build, so every translation unit that includes
    /// it recompiles, and every library and binary above them relinks. One
    /// unconditional action becomes a full rebuild, and the workspace's
    /// incremental builds quietly stop being incremental.
    ///
    /// Rejected at load rather than warned about: a warning here is one nobody
    /// reads until they are measuring why the build is slow, months later. The
    /// fix is usually one character — make the key stable — or to embed the
    /// value at link time instead of compile time.
    fn validate_volatile_stamps(&self) -> Result<()> {
        let mut poisoned: Vec<(usize, &str)> = Vec::new();
        for action in &self.actions {
            if action.kind != ActionKind::Compile {
                continue;
            }
            for &input in action.inputs.iter().chain(&action.order_only_inputs) {
                let Some(producer) = self.files[input].producer else {
                    continue;
                };
                if !self.actions[producer].volatile_stamps.is_empty() {
                    poisoned.push((producer, &self.files[input].path));
                }
            }
        }
        let Some(&(producer, path)) = poisoned.first() else {
            return Ok(());
        };
        let producer = &self.actions[producer];
        let compiles = poisoned.len();
        bail!(
            "action {:?} reads the volatile stamp {} and produces {path:?}, which \
             {compiles} compile action(s) read. A volatile value changes every build, \
             so every one of those compiles — and everything above them — would rebuild \
             every time. Either rename the key to a stable one (it then participates in \
             the action key, and a change rebuilds once), or keep the value out of what \
             compiles read",
            producer.id,
            producer
                .volatile_stamps
                .iter()
                .map(|key| format!("${{stamp.{key}}}"))
                .collect::<Vec<_>>()
                .join(", "),
        )
    }

    fn validate_clean_dirs(&self) -> Result<()> {
        let mut claimed: Vec<(&str, &str)> = Vec::new();
        for action in &self.actions {
            for directory in &action.clean_dirs {
                let path = Path::new(directory);
                for &(other_directory, other_action) in &claimed {
                    let other_path = Path::new(other_directory);
                    if path.starts_with(other_path) || other_path.starts_with(path) {
                        bail!(
                            "clean directory {directory:?} for action {:?} overlaps \
                             {other_directory:?} owned by action {other_action:?}",
                            action.id
                        );
                    }
                }
                for file in &self.files {
                    if Path::new(&file.path).starts_with(path) {
                        bail!(
                            "clean directory {directory:?} for action {:?} contains declared \
                             graph path {:?}; clean_dirs may contain only undeclared \
                             intermediates",
                            action.id,
                            file.path
                        );
                    }
                }
                claimed.push((directory, &action.id));
            }
        }
        Ok(())
    }

    /// All actions needed (transitively) to build the given targets, in a
    /// valid dependency order.
    pub fn action_closure(&self, targets: &[String]) -> Result<Vec<ActionId>> {
        let mut roots: Vec<ActionId> = Vec::new();
        for name in targets {
            let Some(t) = self.targets.get(name) else {
                bail!("unknown target {name:?}");
            };
            roots.extend(t.actions.iter().copied());
        }
        let mut selected = BTreeSet::new();
        let mut stack: Vec<ActionId> = roots;
        while let Some(a) = stack.pop() {
            if !selected.insert(a) {
                continue;
            }
            for &input in self.actions[a]
                .inputs
                .iter()
                .chain(&self.actions[a].order_only_inputs)
            {
                if let Some(producer) = self.files[input].producer {
                    stack.push(producer);
                }
            }
        }
        Ok(selected.into_iter().collect())
    }

    /// Transitive dependency closure of a target, itself included, sorted.
    pub fn deps_closure(&self, root: &str) -> Result<Vec<String>> {
        if !self.targets.contains_key(root) {
            bail!("unknown target {root:?}");
        }
        let mut seen = BTreeSet::new();
        let mut stack = vec![root.to_string()];
        while let Some(name) = stack.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            stack.extend(self.targets[&name].deps.iter().cloned());
        }
        Ok(seen.into_iter().collect())
    }

    /// Transitive reverse-dependency closure: every target that (transitively)
    /// depends on `root`, itself included, sorted. This is the monorepo-CI
    /// primitive ("what does this change affect?").
    pub fn rdeps_closure(&self, root: &str) -> Result<Vec<String>> {
        if !self.targets.contains_key(root) {
            bail!("unknown target {root:?}");
        }
        let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
        for target in self.targets.values() {
            for dep in &target.deps {
                dependents.entry(dep).or_default().push(&target.name);
            }
        }
        let mut seen = BTreeSet::new();
        let mut stack = vec![root];
        while let Some(name) = stack.pop() {
            if !seen.insert(name.to_string()) {
                continue;
            }
            if let Some(users) = dependents.get(name) {
                stack.extend(users.iter().copied());
            }
        }
        Ok(seen.into_iter().collect())
    }

    /// One dependency path from `from` down to `to`, or None when `to` is not
    /// in the dependency closure of `from`.
    pub fn somepath(&self, from: &str, to: &str) -> Result<Option<Vec<String>>> {
        for name in [from, to] {
            if !self.targets.contains_key(name) {
                bail!("unknown target {name:?}");
            }
        }
        fn visit<'a>(
            graph: &'a BuildGraph,
            current: &'a str,
            to: &str,
            path: &mut Vec<String>,
            seen: &mut BTreeSet<&'a str>,
        ) -> bool {
            if !seen.insert(current) {
                return false;
            }
            path.push(current.to_string());
            if current == to {
                return true;
            }
            for dep in &graph.targets[current].deps {
                if visit(graph, dep, to, path, seen) {
                    return true;
                }
            }
            path.pop();
            false
        }
        let mut path = Vec::new();
        let mut seen = BTreeSet::new();
        Ok(visit(self, from, to, &mut path, &mut seen).then_some(path))
    }

    /// Every simple dependency path from `from` down to `to`, sorted.
    ///
    /// `somepath` answers "does this reach that", which is enough to explain a
    /// rebuild. Removing the dependency is the other question, and one path
    /// cannot answer it: cutting the only edge it names leaves the other routes
    /// intact. The count is exponential in the worst case — a chain of diamonds
    /// doubles it per diamond — so the walk stops at `limit` and says that it
    /// did, rather than running until the caller gives up.
    pub fn allpaths(&self, from: &str, to: &str, limit: usize) -> Result<AllPaths> {
        for name in [from, to] {
            if !self.targets.contains_key(name) {
                bail!("unknown target {name:?}");
            }
        }
        fn visit<'a>(
            graph: &'a BuildGraph,
            current: &'a str,
            to: &str,
            path: &mut Vec<&'a str>,
            on_path: &mut BTreeSet<&'a str>,
            found: &mut Vec<Vec<String>>,
            limit: usize,
        ) -> bool {
            // Only the current path is marked, not everything ever visited: a
            // target reachable by two routes must be walked once per route.
            if !on_path.insert(current) {
                return false;
            }
            path.push(current);
            let truncated = if current == to {
                found.push(path.iter().map(|name| (*name).to_string()).collect());
                found.len() >= limit
            } else {
                let mut deps: Vec<&str> = graph.targets[current]
                    .deps
                    .iter()
                    .map(String::as_str)
                    .collect();
                deps.sort_unstable();
                deps.iter()
                    .any(|dep| visit(graph, dep, to, path, on_path, found, limit))
            };
            path.pop();
            on_path.remove(current);
            truncated
        }
        let mut found = Vec::new();
        let truncated = visit(
            self,
            from,
            to,
            &mut Vec::new(),
            &mut BTreeSet::new(),
            &mut found,
            limit.max(1),
        );
        found.sort();
        Ok(AllPaths {
            paths: found,
            truncated,
        })
    }

    /// Targets that declare any of `paths` among the inputs of one of their
    /// actions, sorted. Patterns are globs over workspace-relative paths.
    ///
    /// "Declare" is exact, and narrower than "compiles against": a compile
    /// action's inputs are its source plus the generated headers a genrule
    /// dependency puts in reach, because those are the files the configuration
    /// knows about. A checked-in header only becomes an input of a particular
    /// compile once a build has read the depfile, which is build state rather
    /// than configuration — `frost explain` reports that. Asking for the target
    /// that publishes such a header and then taking its `rdeps` gives the
    /// affected set without leaving the configuration.
    pub fn owners(&self, patterns: &[String]) -> Result<Vec<String>> {
        let matchers = patterns
            .iter()
            .map(|pattern| PathPattern::new(pattern))
            .collect::<Result<Vec<_>>>()?;
        let mut matched_files: BTreeSet<FileId> = BTreeSet::new();
        for (id, file) in self.files.iter().enumerate() {
            if matchers.iter().any(|m| m.matches(&file.path)) {
                matched_files.insert(id);
            }
        }
        let mut owners = BTreeSet::new();
        for action in &self.actions {
            if action
                .inputs
                .iter()
                .chain(&action.order_only_inputs)
                .any(|id| matched_files.contains(id))
            {
                owners.insert(action.target.clone());
            }
        }
        Ok(owners.into_iter().collect())
    }

    pub fn to_dot(&self) -> String {
        let mut out = String::from("digraph frost {\n  rankdir=LR;\n");
        for target in self.targets.values() {
            let shape = match target.kind {
                TargetKind::CcBinary => "box",
                TargetKind::CcLibrary => "ellipse",
                TargetKind::CcTest => "box3d",
                TargetKind::Genrule => "diamond",
                TargetKind::Test => "component",
                TargetKind::KofunBinary => "box",
                TargetKind::Command => "folder",
            };
            out.push_str(&format!("  \"{}\" [shape={shape}];\n", target.name));
            for dep in &target.deps {
                out.push_str(&format!("  \"{}\" -> \"{dep}\";\n", target.name));
            }
        }
        out.push_str("}\n");
        out
    }
}

fn path_key(label: &str) -> String {
    label.trim_start_matches("//").replace([':', '/'], "_")
}

/// Native binary outputs carry the executable suffix expected by the host
/// driver Frost invokes. Platform overlays currently describe toolchains, not
/// a target operating system, so they retain the host driver's naming rule.
fn binary_path(tree: &str, label: &str) -> String {
    format!(
        "{BIN_DIR}/{tree}/{}{}",
        path_key(label),
        std::env::consts::EXE_SUFFIX
    )
}

fn is_cxx_source(path: &str) -> bool {
    matches!(
        PathExt::extension(path),
        Some("cc" | "cpp" | "cxx" | "C" | "c++")
    )
}

struct PathExt;
impl PathExt {
    fn extension(path: &str) -> Option<&str> {
        path.rsplit_once('.').map(|(_, ext)| ext)
    }
}

/// Declared outputs of `dep`, per label, for `${dep:...}` / `${deps:...}`.
///
/// The tree stamp Frost writes for an owned `output_dirs` target is excluded:
/// it is Frost's record of the directory's contents, not a path any tool should
/// be handed. A target that owns only directories therefore resolves to nothing
/// here, and referencing it is an error that says so.
fn dep_output_paths(graph: &BuildGraph, dep: &str) -> Vec<String> {
    graph
        .targets
        .get(dep)
        .map(|target| {
            target
                .outputs
                .iter()
                .map(|&id| graph.files[id].path.clone())
                .filter(|path| !path.starts_with(&format!("{TREE_STAMP_DIR}/")))
                .collect()
        })
        .unwrap_or_default()
}

/// Resolve one `${dep:LABEL}` / `${deps:LABEL}` label against the target's
/// declared dependencies.
///
/// Only declared dependencies resolve. Reaching an arbitrary target would make
/// the argv depend on a target this one has no edge to, so the build could run
/// before that output existed.
///
/// `context` names the place the reference was written — an argv item, a
/// genrule `cmd`, an `env` value. The same three mistakes are possible in all
/// of them and a message that always said "command arg" would point at the
/// wrong line for two of the three.
fn dep_reference<'a>(
    map: &'a [(String, Vec<String>)],
    label: &str,
    context: &str,
) -> Result<&'a [String]> {
    let Some((_, outputs)) = map.iter().find(|(name, _)| name == label) else {
        let declared: Vec<&str> = map.iter().map(|(name, _)| name.as_str()).collect();
        bail!(
            "{context} references {label:?}, which is not a declared dependency \
             (declared: {})",
            if declared.is_empty() {
                "none".to_string()
            } else {
                declared.join(", ")
            }
        );
    };
    if outputs.is_empty() {
        bail!(
            "{context} references {label:?}, which declares no file outputs; \
             a target that owns only output_dirs has no path to substitute"
        );
    }
    Ok(outputs)
}

/// Replace every `${dep:LABEL}` in one string. Single-valued by definition:
/// a dependency with several outputs has no one path this could mean, so it is
/// an error rather than a silent first-wins pick.
fn expand_dep_singles(text: &str, map: &[(String, Vec<String>)], context: &str) -> Result<String> {
    const OPEN: &str = "${dep:";
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        let after = &rest[start + OPEN.len()..];
        let Some(end) = after.find('}') else {
            bail!("unterminated ${{dep:...}} in {context}");
        };
        let label = &after[..end];
        let outputs = dep_reference(map, label, context)?;
        if outputs.len() != 1 {
            bail!(
                "{context} uses ${{dep:{label}}} but {label:?} declares {} outputs; \
                 use ${{deps:{label}}} as a whole argument to pass all of them",
                outputs.len()
            );
        }
        out.push_str(&outputs[0]);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Resolve `${dep:LABEL}` inside `env` values.
///
/// Only the single-valued form. An argv item that expands to several paths
/// becomes several arguments, which is a thing argv can express; an
/// environment variable is one string, and choosing a separator for it — `:`,
/// `;`, a space — would be the string-function language this deliberately is
/// not. So `${deps:LABEL}` says so here rather than inventing one.
///
/// Everything else passes through untouched. An `env` value is an opaque
/// string handed to another program, and programs legitimately want `${...}`
/// in one: rejecting unknown variables the way argv does would break values
/// that were never addressed to Frost.
fn expand_env_dep_refs(
    env: &BTreeMap<String, String>,
    map: &[(String, Vec<String>)],
    target: &str,
) -> Result<BTreeMap<String, String>> {
    let mut expanded = BTreeMap::new();
    for (key, value) in env {
        let context = format!("env {key:?} of target {target:?}");
        if value.contains("${deps:") {
            bail!(
                "{context} uses ${{deps:...}}, which can name several paths; \
                 an environment variable is one string and Frost does not choose \
                 a separator for you — pass them as arguments, or name one output \
                 with ${{dep:LABEL}}"
            );
        }
        expanded.insert(key.clone(), expand_dep_singles(value, map, &context)?);
    }
    Ok(expanded)
}

/// One slice of a sharded test: its action identity, its success stamp, and
/// the environment that tells the runner which slice to run.
struct TestShard {
    id: String,
    desc: String,
    stamp: String,
    /// Where this shard's `.gcda` counters go. Per shard, not per target: it is
    /// reset before every execution, so two shards sharing one would each empty
    /// the other's data as they started.
    gcda: String,
    /// Content-sensitive graph edge for the otherwise dynamically named files
    /// in `gcda`.
    coverage_stamp: String,
    env: BTreeMap<String, String>,
}

/// The name of the object file compiled from `src`, without its directory.
///
/// Coverage flattens the source path into it — `src/a/util.c` becomes
/// `src@a@util.c` — for a reason that is entirely gcc's: a `.gcda` is named
/// after the object's *base* name once `GCOV_PREFIX_STRIP` has removed the
/// directories, so `a/util.c` and `b/util.c` in one target would both write
/// `util.c.gcda`. gcc notices ("overwriting an existing profile data with a
/// different checksum") and one of the two is lost, which is a silently
/// incomplete report rather than a failure.
///
/// Only coverage builds pay for it: an ordinary build keeps the readable
/// mirrored tree, and the two live in different output trees anyway. The
/// target is part of the digest because `GCOV_PREFIX_STRIP` also removes the
/// target directory: two linked targets compiling the same source must not
/// write one flattened counter file. A source basename remains in front so a
/// report directory is still inspectable by a person.
fn object_key(target: &str, src: &str, coverage: bool) -> String {
    match coverage {
        true => {
            let mut hasher = blake3::Hasher::new();
            hasher.update(target.as_bytes());
            hasher.update(b"\0");
            hasher.update(src.as_bytes());
            let digest = hasher.finalize().to_hex();
            let basename = src.rsplit('/').next().unwrap_or(src);
            format!("{basename}-{}", &digest[..16])
        }
        false => src.to_string(),
    }
}

/// The `.gcno` gcc writes for `obj`.
///
/// It replaces the object's extension rather than appending to it, so
/// `x/main.c.o` yields `x/main.c.gcno`. Taken from what gcc does, not from what
/// its documentation says about it.
fn notes_path(obj: &str) -> String {
    format!("{}.gcno", obj.strip_suffix(".o").unwrap_or(obj))
}

/// Split a test target into `total` shards.
///
/// Frost does not divide the test cases — it cannot know them — it divides the
/// work by telling the runner which slice is its own, using the protocol test
/// runners already implement. A runner that ignores the variables runs every
/// case in every shard, which is why `shard_count` is declared per target
/// rather than applied by Frost on its own.
///
/// `total == 1` reproduces exactly the identity, stamp and empty shard
/// environment Frost has always used, so leaving the field out — or writing
/// `shard_count = 1` — cannot invalidate an existing journal.
fn test_shards(tree: &str, name: &str, total: u32) -> Vec<TestShard> {
    let key = path_key(name);
    if total <= 1 {
        return vec![TestShard {
            id: format!("test:{name}"),
            desc: format!("TEST {name}"),
            stamp: format!(".frost/test/{tree}/{key}/passed"),
            gcda: format!("{COVERAGE_DIR}/{tree}/{key}/gcda"),
            coverage_stamp: format!("{TREE_STAMP_DIR}/{tree}/coverage/{key}/contents"),
            env: BTreeMap::new(),
        }];
    }
    (0..total)
        .map(|index| {
            let dir = format!(".frost/test/{tree}/{key}/shard-{index}-of-{total}");
            let status = format!("{dir}/status");
            TestShard {
                id: format!("test:{name}#{index}/{total}"),
                desc: format!("TEST {name} (shard {}/{total})", index + 1),
                stamp: format!("{dir}/passed"),
                gcda: format!("{COVERAGE_DIR}/{tree}/{key}/shard-{index}-of-{total}/gcda"),
                coverage_stamp: format!(
                    "{TREE_STAMP_DIR}/{tree}/coverage/{key}/shard-{index}-of-{total}/contents"
                ),
                env: BTreeMap::from([
                    ("TEST_SHARD_INDEX".to_string(), index.to_string()),
                    ("TEST_TOTAL_SHARDS".to_string(), total.to_string()),
                    ("TEST_SHARD_STATUS_FILE".to_string(), status),
                    // googletest reads its own spelling, and a gtest binary is
                    // the most common thing a cc_test contains, so it shards
                    // without a wrapper script.
                    ("GTEST_SHARD_INDEX".to_string(), index.to_string()),
                    ("GTEST_TOTAL_SHARDS".to_string(), total.to_string()),
                ]),
            }
        })
        .collect()
}

/// Merge the shard environment into a target's own, refusing a collision.
///
/// A target that sets `TEST_SHARD_INDEX` itself has a different intent than
/// the one sharding implies, and quietly overriding either direction would
/// make one of them a lie.
fn merge_shard_env(
    target_env: &BTreeMap<String, String>,
    pass_env: &[String],
    shard: &BTreeMap<String, String>,
    name: &str,
) -> Result<BTreeMap<String, String>> {
    let mut env = target_env.clone();
    for (key, value) in shard {
        if target_env.contains_key(key) || pass_env.iter().any(|passed| passed == key) {
            bail!(
                "test {name:?} declares {key} and also shard_count; \
                 sharding sets that variable itself"
            );
        }
        env.insert(key.clone(), value.clone());
    }
    Ok(env)
}

fn dep_outputs(graph: &BuildGraph, dep: &str) -> Vec<FileId> {
    graph
        .targets
        .get(dep)
        .map(|t| t.outputs.clone())
        .unwrap_or_default()
}

/// Persistent ordered string set: own entries plus references to parent sets,
/// shared structurally across targets so transitive export propagation costs
/// O(targets + edges) instead of materializing a flat closure per target (#78).
///
/// `flatten` walks own entries first, then parents in declaration order
/// (iterative preorder, first occurrence wins) — exactly the ordering the
/// historical flattened-Vec code produced, so action argv and cache keys are
/// unchanged by the representation.
struct SharedSet {
    own: Vec<String>,
    parents: Vec<Rc<SharedSet>>,
}

impl SharedSet {
    fn join(own: Vec<String>, parents: Vec<Rc<SharedSet>>) -> Rc<Self> {
        Rc::new(Self { own, parents })
    }

    fn flatten(self: &Rc<Self>) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut visited: HashSet<*const SharedSet> = HashSet::new();
        let mut stack: Vec<Rc<SharedSet>> = vec![Rc::clone(self)];
        while let Some(node) = stack.pop() {
            if !visited.insert(Rc::as_ptr(&node)) {
                continue;
            }
            for value in &node.own {
                if seen.insert(value.clone()) {
                    out.push(value.clone());
                }
            }
            for parent in node.parents.iter().rev() {
                stack.push(Rc::clone(parent));
            }
        }
        out
    }
}

/// Depth-first topological sort over target deps with cycle reporting.
fn toposort_targets(manifest: &Manifest) -> Result<Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Unvisited,
        Visiting,
        Done,
    }

    fn visit(
        name: &str,
        manifest: &Manifest,
        states: &mut BTreeMap<String, State>,
        path: &mut Vec<String>,
        order: &mut Vec<String>,
    ) -> Result<()> {
        // `Manifest::load` rejects an unknown dependency before anything is
        // configured, so this is an invariant rather than user input. It is
        // still checked: `Manifest::load_reporting` hands out manifests that
        // did not pass that check, and a panic deep in a topological sort is a
        // poor way to find out.
        let Some(state) = states.get(name) else {
            bail!("target {name:?} is not declared, so the graph cannot be ordered");
        };
        match state {
            State::Done => return Ok(()),
            State::Visiting => {
                let start = path.iter().position(|p| p == name).unwrap_or(0);
                let mut cycle = path[start..].to_vec();
                cycle.push(name.to_string());
                bail!("dependency cycle: {}", cycle.join(" -> "));
            }
            State::Unvisited => {}
        }
        states.insert(name.to_string(), State::Visiting);
        path.push(name.to_string());
        for dep in &manifest.targets[name].deps {
            visit(dep, manifest, states, path, order)?;
        }
        path.pop();
        states.insert(name.to_string(), State::Done);
        order.push(name.to_string());
        Ok(())
    }

    let mut states: BTreeMap<String, State> = manifest
        .targets
        .keys()
        .map(|k| (k.clone(), State::Unvisited))
        .collect();
    let mut order = Vec::new();
    let mut path = Vec::new();
    let names: Vec<String> = manifest.targets.keys().cloned().collect();
    for name in names {
        visit(&name, manifest, &mut states, &mut path, &mut order)?;
    }
    Ok(order)
}

fn expand_genrule_cmd(
    cmd: &str,
    inputs: &[String],
    outputs: &[String],
    dependency_map: &[(String, Vec<String>)],
    target: &str,
) -> Result<String> {
    let context = format!("genrule cmd of target {target:?}");
    // `cmd` is one shell string, so the plural forms join on a space the way
    // `${in}` and `${outs}` already do. That is the shell's own separator here,
    // not a joiner Frost invented — which is why the same form is refused in
    // `env`, where there is no such convention to borrow.
    let mut expanded = String::new();
    let mut rest = cmd;
    while let Some(start) = rest.find("${deps:") {
        expanded.push_str(&rest[..start]);
        let after = &rest[start + "${deps:".len()..];
        let Some(end) = after.find('}') else {
            bail!("unterminated ${{deps:...}} in {context}");
        };
        let paths = dep_reference(dependency_map, &after[..end], &context)?;
        expanded.push_str(&paths.join(" "));
        rest = &after[end + 1..];
    }
    expanded.push_str(rest);

    let expanded = expand_dep_singles(&expanded, dependency_map, &context)?;
    let expanded = expanded
        .replace("${in}", &inputs.join(" "))
        .replace("${outs}", &outputs.join(" "))
        .replace("${out}", &outputs[0])
        .replace("${pathsep}", std::path::MAIN_SEPARATOR_STR);
    if expanded.contains(STAMP_OPEN) {
        // Worth its own sentence: a genrule is the obvious place to write a
        // version header, and "unknown variable" would send the author looking
        // for a typo in a name that is spelled correctly.
        bail!(
            "genrule cmd {cmd:?} uses ${{stamp.…}}, which only `kind = \"command\"` \
             targets expand. A genrule runs through a shell, where frost cannot \
             tell a value it substituted from one the shell produced"
        );
    }
    if expanded.contains("${") {
        bail!(
            "genrule cmd has unknown variable: {cmd:?} \
             (supported: ${{in}}, ${{out}}, ${{outs}}, ${{pathsep}}, \
             ${{dep:LABEL}}, ${{deps:LABEL}})"
        );
    }
    Ok(expanded)
}

/// Which stamp keys a command reads, split by whether the value is action-key
/// material. Sorted and deduplicated so the pair is a property of the action
/// rather than of the order its arguments happen to be written in.
fn collect_stamps<'a>(
    texts: impl Iterator<Item = &'a String>,
    stamp: Option<&crate::manifest::Stamp>,
    target: &str,
) -> Result<(Vec<String>, Vec<String>)> {
    let mut stable = Vec::new();
    let mut volatile = Vec::new();
    for text in texts {
        for key in crate::stamp::references(text, &format!("target {target:?}"))? {
            let Some(stamp) = stamp else {
                bail!(
                    "target {target:?} references ${{stamp.{key}}}, but this workspace \
                     declares no [stamp] section"
                );
            };
            match crate::stamp::is_stable(&key, &stamp.stable_prefix) {
                true => stable.push(key),
                false => volatile.push(key),
            }
        }
    }
    for keys in [&mut stable, &mut volatile] {
        keys.sort_unstable();
        keys.dedup();
    }
    Ok((stable, volatile))
}

/// `${stamp.KEY}` is expanded at execution time, not here, so a check for
/// leftover variables must not read one as a typo. Callers that expand a
/// *path* pass `false`: a stamp in an output path would name a different file
/// every build, which is never what was meant.
fn unresolved_variable(text: &str, allow_stamp: bool) -> bool {
    let scanned = match allow_stamp {
        true => std::borrow::Cow::Owned(text.replace(STAMP_OPEN, "")),
        false => std::borrow::Cow::Borrowed(text),
    };
    scanned.contains("${")
}

fn expand_config_template(
    value: &str,
    config: &str,
    profile: &str,
    platform: &str,
    allow_stamp: bool,
) -> Result<String> {
    let expanded = value
        .replace("${config}", config)
        .replace("${profile}", profile)
        .replace("${platform}", platform);
    if unresolved_variable(&expanded, allow_stamp) {
        bail!(
            "unknown configuration variable in {value:?} \
             (supported: ${{config}}, ${{profile}}, ${{platform}})"
        );
    }
    Ok(expanded)
}

#[allow(clippy::too_many_arguments)]
fn expand_command_args(
    driver: &str,
    args: &[String],
    inputs: &[String],
    dependency_inputs: &[String],
    dependency_map: &[(String, Vec<String>)],
    outputs: &[String],
    output_dirs: &[String],
    clean_dirs: &[String],
    depfile: Option<&str>,
    config: &str,
    profile: &str,
    platform: &str,
) -> Result<Vec<String>> {
    let mut argv = vec![driver.to_string()];
    for arg in args {
        if let Some(label) = arg
            .strip_prefix("${deps:")
            .and_then(|rest| rest.strip_suffix('}'))
        {
            argv.extend(
                dep_reference(dependency_map, label, &format!("command arg {arg:?}"))?
                    .iter()
                    .cloned(),
            );
            continue;
        }
        match arg.as_str() {
            "${in}" => argv.extend(inputs.iter().cloned()),
            "${deps}" => argv.extend(dependency_inputs.iter().cloned()),
            "${outs}" => argv.extend(outputs.iter().cloned()),
            "${output_dirs}" => argv.extend(output_dirs.iter().cloned()),
            "${clean_dirs}" => argv.extend(clean_dirs.iter().cloned()),
            _ => {
                if arg.contains("${in}")
                    || arg.contains("${deps}")
                    || arg.contains("${outs}")
                    || arg.contains("${output_dirs}")
                    || arg.contains("${clean_dirs}")
                    || arg.contains("${deps:")
                {
                    bail!(
                        "multi-value command variables must occupy one complete argument: {arg:?}"
                    );
                }
                let mut expanded =
                    expand_dep_singles(arg, dependency_map, &format!("command arg {arg:?}"))?;
                if expanded.contains("${out}") || expanded.contains("${out_dir}") {
                    // A target that only owns directories has no single output
                    // path to name, which is a manifest error rather than a
                    // reason to index into an empty list.
                    let Some(first) = outputs.first() else {
                        bail!(
                            "command arg {arg:?} uses ${{out}}/${{out_dir}} but the target                              declares no outputs (use ${{output_dir}} for an owned directory)"
                        );
                    };
                    let output_dir = first
                        .rsplit_once('/')
                        .map_or(".", |(directory, _)| directory);
                    expanded = expanded
                        .replace("${out_dir}", output_dir)
                        .replace("${out}", first);
                }
                if expanded.contains("${output_dir}") {
                    let Some(first) = output_dirs.first() else {
                        bail!("command arg uses ${{output_dir}} but no output_dirs are declared");
                    };
                    expanded = expanded.replace("${output_dir}", first);
                }
                if expanded.contains("${depfile}") {
                    let Some(depfile) = depfile else {
                        bail!("command arg uses ${{depfile}} but no depfile is configured");
                    };
                    expanded = expanded.replace("${depfile}", depfile);
                }
                if expanded.contains("${clean_dir}") {
                    let Some(clean_dir) = clean_dirs.first() else {
                        bail!("command arg uses ${{clean_dir}} but no clean_dirs are configured");
                    };
                    expanded = expanded.replace("${clean_dir}", clean_dir);
                }
                expanded = expand_config_template(&expanded, config, profile, platform, true)?;
                if unresolved_variable(&expanded, true) {
                    bail!(
                        "unknown command variable in {arg:?} (supported: ${{in}}, ${{deps}}, \
                         ${{dep:LABEL}}, ${{deps:LABEL}}, \
                         ${{out}}, ${{out_dir}}, ${{outs}}, ${{output_dir}}, \
                         ${{output_dirs}}, ${{clean_dir}}, ${{clean_dirs}}, \
                         ${{depfile}}, ${{config}}, ${{profile}}, ${{platform}}, \
                         ${{stamp.KEY}})"
                    );
                }
                argv.push(expanded);
            }
        }
    }
    Ok(argv)
}

#[allow(clippy::too_many_arguments)]
fn expand_test_args(
    driver: &str,
    args: &[String],
    inputs: &[String],
    dependency_inputs: &[String],
    dependency_map: &[(String, Vec<String>)],
    config: &str,
    profile: &str,
    platform: &str,
) -> Result<Vec<String>> {
    let mut argv = vec![driver.to_string()];
    for arg in args {
        if let Some(label) = arg
            .strip_prefix("${deps:")
            .and_then(|rest| rest.strip_suffix('}'))
        {
            argv.extend(
                dep_reference(dependency_map, label, &format!("test arg {arg:?}"))?
                    .iter()
                    .cloned(),
            );
            continue;
        }
        match arg.as_str() {
            "${in}" => argv.extend(inputs.iter().cloned()),
            "${deps}" => argv.extend(dependency_inputs.iter().cloned()),
            _ => {
                if arg.contains("${in}") || arg.contains("${deps}") || arg.contains("${deps:") {
                    bail!("multi-value test variables must occupy one complete argument: {arg:?}");
                }
                let expanded =
                    expand_dep_singles(arg, dependency_map, &format!("test arg {arg:?}"))?;
                let expanded = expand_config_template(&expanded, config, profile, platform, true)?;
                argv.push(expanded);
            }
        }
    }
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;

    fn demo_manifest() -> Manifest {
        Manifest::parse_str(
            r#"
            [toolchain]
            cc = "cc"

            [target.gen]
            kind = "genrule"
            cmd = "sh gen.sh ${out}"
            inputs = ["gen.sh"]
            outputs = ["gen/config.h"]
            includes = ["gen"]

            [target.util]
            kind = "cc_library"
            srcs = ["src/util.c"]
            includes = ["include"]

            [target.app]
            kind = "cc_binary"
            srcs = ["src/main.c"]
            deps = ["util", "gen"]
            "#,
        )
        .unwrap()
    }

    #[test]
    fn builds_expected_actions() {
        let graph = BuildGraph::from_manifest(&demo_manifest()).unwrap();
        let ids: Vec<&str> = graph.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains(&"genrule:gen"));
        assert!(ids.contains(&"compile:util:src/util.c"));
        assert!(ids.contains(&"archive:util"));
        assert!(ids.contains(&"compile:app:src/main.c"));
        assert!(ids.contains(&"link:app"));
    }

    #[test]
    fn target_resources_reach_every_action_without_changing_graph_shape() {
        let manifest = Manifest::parse_str(
            r#"
            [target.heavy]
            kind = "cc_binary"
            srcs = ["a.c", "b.c"]
            resources = { cpu = 2, ram_mb = 2048, exclusive = true }
            "#,
        )
        .unwrap();
        let graph = BuildGraph::from_manifest(&manifest).unwrap();
        assert_eq!(graph.actions.len(), 3, "two compiles and one link");
        assert!(graph.actions.iter().all(|action| action.resources
            == crate::manifest::ActionResources {
                cpu: 2,
                ram_mb: 2048,
                exclusive: true,
            }));
    }

    #[test]
    fn coverage_is_a_content_keyed_configuration_with_one_merge_per_test() {
        // Deliberately compile the same source in a library and the test that
        // links it. GCOV_PREFIX_STRIP flattens both object directories, so the
        // object basenames must carry the target identity or their counters
        // collide at runtime.
        let manifest = Manifest::parse_str(
            r#"
            [target.lib]
            kind = "cc_library"
            srcs = ["shared.c"]

            [target.unit]
            kind = "cc_test"
            srcs = ["shared.c"]
            deps = ["lib"]
            "#,
        )
        .unwrap();
        let ordinary = BuildGraph::from_manifest(&manifest).unwrap();
        assert!(!ordinary.coverage);
        assert!(ordinary
            .actions
            .iter()
            .all(|action| action.kind != ActionKind::Coverage));

        let graph = BuildGraph::from_manifest_instrumented(
            &manifest,
            "debug",
            crate::manifest::HOST_PLATFORM,
            true,
        )
        .unwrap();
        assert!(graph.coverage);

        let compiles = graph
            .actions
            .iter()
            .filter(|action| action.kind == ActionKind::Compile)
            .collect::<Vec<_>>();
        assert_eq!(compiles.len(), 2);
        assert!(compiles
            .iter()
            .all(|action| action.argv.iter().any(|arg| arg == COVERAGE_FLAG)));
        let note_names = compiles
            .iter()
            .flat_map(|action| action.outputs.iter())
            .map(|&file| &graph.files[file].path)
            .filter(|path| path.ends_with(".gcno"))
            .filter_map(|path| Path::new(path).file_name())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            note_names.len(),
            2,
            "linked targets must write distinct flattened counter names"
        );

        let test = graph
            .actions
            .iter()
            .find(|action| action.id == "test:unit")
            .unwrap();
        assert_eq!(test.clean_dirs, test.output_dirs);
        assert_eq!(test.clean_dirs.len(), 1);
        let counter_stamp = test
            .outputs
            .iter()
            .copied()
            .find(|&file| graph.files[file].path.starts_with(TREE_STAMP_DIR))
            .expect("the raw counter tree needs a content stamp");

        let merge = graph
            .actions
            .iter()
            .find(|action| action.id == "coverage:unit")
            .unwrap();
        assert!(merge.inputs.contains(&counter_stamp));
        let spec = merge.coverage.as_ref().unwrap();
        assert_eq!(spec.notes.len(), 2);
        assert_eq!(spec.output, ".frost/coverage/debug+coverage/unit.lcov");
        assert_eq!(
            graph.targets["unit"].actions.last(),
            Some(&(graph.actions.len() - 1))
        );
    }

    #[test]
    fn genrule_path_separator_matches_the_host_shell() {
        let expanded = expand_genrule_cmd(
            "tools${pathsep}generate ${out}",
            &[],
            &["out".into()],
            &[],
            "gen",
        )
        .unwrap();
        assert_eq!(
            expanded,
            format!("tools{}generate out", std::path::MAIN_SEPARATOR)
        );
    }

    #[test]
    fn host_binaries_use_the_host_executable_suffix() {
        let graph = BuildGraph::from_manifest(&demo_manifest()).unwrap();
        let link = graph.actions.iter().find(|a| a.id == "link:app").unwrap();
        let expected = format!("{BIN_DIR}/debug/app{}", std::env::consts::EXE_SUFFIX);
        assert!(link.argv.contains(&expected), "argv: {:?}", link.argv);
        assert_eq!(
            graph.targets["app"]
                .outputs
                .iter()
                .map(|&id| graph.files[id].path.as_str())
                .collect::<Vec<_>>(),
            vec![expected.as_str()]
        );
    }

    #[test]
    fn kofun_binary_is_one_cacheable_action_with_declared_artifacts() {
        let manifest = Manifest::parse_str(
            r#"
            [toolchain]
            kofunc = "tools/kofun"

            [target.generated]
            kind = "genrule"
            cmd = "generate ${out}"
            inputs = ["schema.txt"]
            outputs = ["generated/data.txt"]

            [target.app]
            kind = "kofun_binary"
            srcs = ["src/main.kofun"]
            deps = ["generated"]
            "#,
        )
        .unwrap();
        let graph = BuildGraph::from_manifest(&manifest).unwrap();
        let action = graph.actions.iter().find(|a| a.id == "kofun:app").unwrap();
        let host_bin = format!(".frost/bin/debug/app{}", std::env::consts::EXE_SUFFIX);
        assert_eq!(action.kind, ActionKind::KofunCompile);
        assert_eq!(
            action.argv,
            vec![
                "tools/kofun".to_string(),
                "build".to_string(),
                "src/main.kofun".to_string(),
                "-o".to_string(),
                host_bin.clone(),
                "--emit-c".to_string(),
                ".frost/obj/debug/app/kofun.c".to_string(),
            ]
        );
        assert_eq!(action.depfile, None);
        let inputs = action
            .inputs
            .iter()
            .map(|&id| graph.files[id].path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(inputs, vec!["src/main.kofun", "generated/data.txt"]);
        let outputs = action
            .outputs
            .iter()
            .map(|&id| graph.files[id].path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            outputs,
            vec![host_bin.as_str(), ".frost/obj/debug/app/kofun.c"]
        );
        assert_eq!(
            graph.targets["app"]
                .outputs
                .iter()
                .map(|&id| graph.files[id].path.as_str())
                .collect::<Vec<_>>(),
            vec![host_bin.as_str()]
        );
        assert!(
            graph.to_dot().contains("\"app\" [shape=box]"),
            "{}",
            graph.to_dot()
        );
    }

    #[test]
    fn command_target_expands_direct_argv_and_configuration_paths() {
        let manifest = Manifest::parse_str(
            r#"
            [toolchain.tools]
            runner = "tools/runner"
            packer = "tools/packer"

            [platform.device.tools]
            runner = "tools/device-runner"
            packer = "tools/device-packer"

            [target.generate]
            kind = "genrule"
            cmd = "generate ${out}"
            inputs = ["schema.txt"]
            outputs = ["generated/data.txt"]

            [target.app]
            kind = "command"
            tool = "runner"
            args = ["--input", "${in}", "--deps", "${deps}", "--output", "${out}",
                    "--output-dir", "${out_dir}", "--depfile", "${depfile}",
                    "--temp", "${clean_dir}", "--platform", "${platform}"]
            inputs = ["src/app.lang"]
            outputs = [".frost/out/${config}/app.bin"]
            depfile = ".frost/out/${config}/app.d"
            clean_dirs = [".frost/tmp/${config}/app"]
            preserve_outputs = true
            steps = [{ tool = "packer", args = ["${out}", "${clean_dirs}", "${config}"] }]
            deps = ["generate"]
            env = { MODE = "release" }
            pass_env = ["LANG_HOME"]
            sandbox = false
            "#,
        )
        .unwrap();
        let graph = BuildGraph::from_manifest_configured(&manifest, "debug", "device").unwrap();
        let action = graph
            .actions
            .iter()
            .find(|action| action.id == "command:app")
            .unwrap();
        assert_eq!(action.kind, ActionKind::Command);
        assert_eq!(
            action.argv,
            vec![
                "tools/device-runner",
                "--input",
                "src/app.lang",
                "--deps",
                "generated/data.txt",
                "--output",
                ".frost/out/device/debug/app.bin",
                "--output-dir",
                ".frost/out/device/debug",
                "--depfile",
                ".frost/out/device/debug/app.d",
                "--temp",
                ".frost/tmp/device/debug/app",
                "--platform",
                "device",
            ]
        );
        assert_eq!(action.env["MODE"], "release");
        assert_eq!(action.pass_env, vec!["LANG_HOME"]);
        assert!(action.preserve_outputs);
        assert_eq!(
            action.followup_argv,
            vec![vec![
                "tools/device-packer",
                ".frost/out/device/debug/app.bin",
                ".frost/tmp/device/debug/app",
                "device/debug",
            ]]
        );
        assert_eq!(action.clean_dirs, vec![".frost/tmp/device/debug/app"]);
        assert_eq!(
            action.depfile.as_deref(),
            Some(".frost/out/device/debug/app.d")
        );
        let inputs = action
            .inputs
            .iter()
            .map(|&id| graph.files[id].path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            inputs,
            vec![
                "src/app.lang",
                "tools/device-runner",
                "generated/data.txt",
                "tools/device-packer"
            ]
        );
    }

    #[test]
    fn direct_test_uses_named_tool_and_executor_owned_success_stamp() {
        let manifest = Manifest::parse_str(
            r#"
            [toolchain.tools]
            python = "tools/python"

            [target.generated]
            kind = "genrule"
            cmd = "generate ${out}"
            inputs = ["schema.txt"]
            outputs = ["generated/value.py"]

            [target.unit]
            kind = "test"
            tool = "python"
            args = ["tests/unit.py", "${in}", "${deps}", "${profile}"]
            inputs = ["tests/unit.py"]
            deps = ["generated"]
            env = { PYTHONHASHSEED = "0" }
            pass_env = ["PYTHONPATH"]
            sandbox = false
            "#,
        )
        .unwrap();
        let graph = BuildGraph::from_manifest(&manifest).unwrap();
        let action = graph
            .actions
            .iter()
            .find(|action| action.id == "test:unit")
            .unwrap();
        assert_eq!(action.kind, ActionKind::Test);
        assert_eq!(
            action.argv,
            [
                "tools/python",
                "tests/unit.py",
                "tests/unit.py",
                "generated/value.py",
                "debug"
            ]
        );
        assert_eq!(action.env["PYTHONHASHSEED"], "0");
        assert_eq!(action.pass_env, ["PYTHONPATH"]);
        assert!(action.followup_argv.is_empty());
        assert_eq!(
            graph.files[action.outputs[0]].path,
            ".frost/test/debug/unit/passed"
        );
    }

    #[test]
    fn command_clean_dirs_cannot_overlap_between_actions() {
        let manifest = Manifest::parse_str(
            r#"
            [toolchain.tools]
            runner = "runner"

            [target.first]
            kind = "command"
            tool = "runner"
            args = ["${out}"]
            outputs = [".frost/out/${config}/first.bin"]
            clean_dirs = [".frost/tmp/${config}/shared"]

            [target.second]
            kind = "command"
            tool = "runner"
            args = ["${out}"]
            outputs = [".frost/out/${config}/second.bin"]
            clean_dirs = [".frost/tmp/${config}/shared/nested"]
            "#,
        )
        .unwrap();
        let error = BuildGraph::from_manifest(&manifest)
            .unwrap_err()
            .to_string();
        assert!(error.contains("overlaps"), "{error}");
        assert!(error.contains("command:first"), "{error}");
        assert!(error.contains("command:second"), "{error}");
    }

    #[test]
    fn command_clean_dirs_cannot_contain_declared_graph_paths() {
        let manifest = Manifest::parse_str(
            r#"
            [toolchain.tools]
            runner = "runner"

            [target.app]
            kind = "command"
            tool = "runner"
            args = ["${out}"]
            outputs = [".frost/tmp/${config}/app/final.bin"]
            clean_dirs = [".frost/tmp/${config}/app"]
            "#,
        )
        .unwrap();
        let error = BuildGraph::from_manifest(&manifest)
            .unwrap_err()
            .to_string();
        assert!(error.contains("contains declared graph path"), "{error}");
        assert!(error.contains("undeclared intermediates"), "{error}");
    }

    #[test]
    fn command_clean_dir_placeholder_requires_an_owned_directory() {
        let manifest = Manifest::parse_str(
            r#"
            [toolchain.tools]
            runner = "runner"

            [target.app]
            kind = "command"
            tool = "runner"
            args = ["--temp", "${clean_dir}", "${out}"]
            outputs = [".frost/out/${config}/app.bin"]
            "#,
        )
        .unwrap();
        let error = BuildGraph::from_manifest(&manifest)
            .unwrap_err()
            .to_string();
        assert!(error.contains("no clean_dirs are configured"), "{error}");
    }

    #[test]
    fn kofun_binary_requires_an_explicit_compiler() {
        let manifest =
            Manifest::parse_str("[target.app]\nkind='kofun_binary'\nsrcs=['main.kofun']\n")
                .unwrap();
        let error = BuildGraph::from_manifest(&manifest)
            .unwrap_err()
            .to_string();
        assert!(error.contains("does not configure kofunc"), "{error}");
    }

    #[test]
    fn compile_gets_dep_includes_and_gen_inputs() {
        let graph = BuildGraph::from_manifest(&demo_manifest()).unwrap();
        let compile = graph
            .actions
            .iter()
            .find(|a| a.id == "compile:app:src/main.c")
            .unwrap();
        assert!(compile.argv.contains(&"-Iinclude".to_string()));
        assert!(compile.argv.contains(&"-Igen".to_string()));
        let input_paths: Vec<&str> = compile
            .order_only_inputs
            .iter()
            .map(|&f| graph.files[f].path.as_str())
            .collect();
        assert!(input_paths.contains(&"gen/config.h"));
    }

    #[test]
    fn link_orders_after_archive() {
        let graph = BuildGraph::from_manifest(&demo_manifest()).unwrap();
        let link = graph.actions.iter().find(|a| a.id == "link:app").unwrap();
        let lib = format!("{LIB_DIR}/debug/libutil.a");
        assert!(link.argv.contains(&lib));
        let input_paths: Vec<&str> = link
            .inputs
            .iter()
            .map(|&f| graph.files[f].path.as_str())
            .collect();
        assert!(input_paths.contains(&lib.as_str()));
    }

    #[test]
    fn closure_selects_only_needed_actions() {
        let graph = BuildGraph::from_manifest(&demo_manifest()).unwrap();
        let closure = graph.action_closure(&["util".to_string()]).unwrap();
        let ids: Vec<&str> = closure
            .iter()
            .map(|&a| graph.actions[a].id.as_str())
            .collect();
        assert!(ids.contains(&"compile:util:src/util.c"));
        assert!(ids.contains(&"archive:util"));
        assert!(!ids.contains(&"link:app"));
        assert!(!ids.contains(&"genrule:gen"));
    }

    #[test]
    fn query_closures_and_somepath() {
        let graph = BuildGraph::from_manifest(&demo_manifest()).unwrap();
        assert_eq!(
            graph.deps_closure("app").unwrap(),
            vec!["app", "gen", "util"]
        );
        assert_eq!(graph.deps_closure("util").unwrap(), vec!["util"]);
        assert_eq!(graph.rdeps_closure("util").unwrap(), vec!["app", "util"]);
        assert_eq!(
            graph.somepath("app", "gen").unwrap(),
            Some(vec!["app".to_string(), "gen".to_string()])
        );
        assert_eq!(graph.somepath("util", "gen").unwrap(), None);
        assert!(graph.deps_closure("nope").is_err());
    }

    fn dep_reference_manifest(user_args: &str) -> String {
        format!(
            r#"
            [toolchain.tools]
            pack = "pack"

            [target.one]
            kind = "genrule"
            cmd = "sh one.sh ${{out}}"
            inputs = ["one.sh"]
            outputs = ["gen/one.txt"]

            [target.two]
            kind = "genrule"
            cmd = "sh two.sh ${{outs}}"
            inputs = ["two.sh"]
            outputs = ["gen/two-a.txt", "gen/two-b.txt"]

            [target.tree]
            kind = "command"
            tool = "pack"
            inputs = ["tree.in"]
            output_dirs = ["dist/${{config}}"]
            args = ["--out", "${{output_dir}}"]

            [target.user]
            kind = "command"
            tool = "pack"
            inputs = ["user.in"]
            outputs = [".frost/out/${{config}}/user.bin"]
            deps = ["one", "two", "tree"]
            args = {user_args}
            "#
        )
    }

    fn user_argv(user_args: &str) -> Result<Vec<String>> {
        let manifest = Manifest::parse_str(&dep_reference_manifest(user_args))?;
        let graph = BuildGraph::from_manifest(&manifest)?;
        let action = graph
            .actions
            .iter()
            .find(|action| action.target == "user")
            .expect("user action");
        Ok(action.argv.clone())
    }

    fn sharded_test_manifest(shard_count: &str, extra: &str) -> String {
        format!(
            r#"
            [target.split]
            kind = "test"
            cmd = "sh run.sh"
            inputs = ["run.sh"]
            {shard_count}
            {extra}
            "#
        )
    }

    #[derive(Debug, PartialEq, Eq)]
    struct TestAction {
        id: String,
        stamp: String,
        env: BTreeMap<String, String>,
        argv: Vec<String>,
        flaky_retries: u32,
    }

    fn test_actions(manifest: &str) -> Result<Vec<TestAction>> {
        let graph = BuildGraph::from_manifest(&Manifest::parse_str(manifest)?)?;
        Ok(graph
            .actions
            .iter()
            .filter(|action| action.kind == ActionKind::Test)
            .map(|action| TestAction {
                id: action.id.clone(),
                stamp: graph.files[action.outputs[0]].path.clone(),
                env: action.env.clone(),
                argv: action.argv.clone(),
                flaky_retries: action.flaky_retries,
            })
            .collect())
    }

    #[test]
    fn an_unsharded_test_keeps_the_identity_it_always_had() {
        // A journal keyed by these strings must survive the feature landing,
        // so both spellings of "not sharded" reproduce the old action exactly.
        let implicit = test_actions(&sharded_test_manifest("", "")).unwrap();
        let explicit = test_actions(&sharded_test_manifest("shard_count = 1", "")).unwrap();
        assert_eq!(implicit, explicit);
        assert_eq!(implicit.len(), 1);
        assert_eq!(implicit[0].id, "test:split");
        assert_eq!(implicit[0].stamp, ".frost/test/debug/split/passed");
        assert!(
            implicit[0].env.is_empty(),
            "an unsharded test gets no shard environment: {:?}",
            implicit[0].env
        );
    }

    #[test]
    fn each_shard_is_a_separate_action_with_its_own_stamp_and_slice() {
        let actions = test_actions(&sharded_test_manifest("shard_count = 3", "")).unwrap();
        assert_eq!(actions.len(), 3);

        let ids: Vec<&str> = actions.iter().map(|action| action.id.as_str()).collect();
        assert_eq!(ids, ["test:split#0/3", "test:split#1/3", "test:split#2/3"]);

        // Distinct stamps are what make the shards cache independently: one
        // shard failing or being invalidated cannot touch another's result.
        let stamps: Vec<&str> = actions.iter().map(|action| action.stamp.as_str()).collect();
        assert_eq!(
            stamps,
            [
                ".frost/test/debug/split/shard-0-of-3/passed",
                ".frost/test/debug/split/shard-1-of-3/passed",
                ".frost/test/debug/split/shard-2-of-3/passed",
            ]
        );

        let env = &actions[1].env;
        assert_eq!(env["TEST_SHARD_INDEX"], "1");
        assert_eq!(env["TEST_TOTAL_SHARDS"], "3");
        assert_eq!(
            env["TEST_SHARD_STATUS_FILE"],
            ".frost/test/debug/split/shard-1-of-3/status"
        );
        // googletest reads its own spelling, so a gtest binary shards without
        // a wrapper.
        assert_eq!(env["GTEST_SHARD_INDEX"], "1");
        assert_eq!(env["GTEST_TOTAL_SHARDS"], "3");

        // The environment differs per shard, and the environment is action-key
        // material, so the shards cannot collide on one cache entry.
        assert_ne!(actions[0].env, actions[1].env);
    }

    #[test]
    fn shard_count_is_rejected_where_it_would_do_nothing_or_contradict() {
        let error = Manifest::parse_str(
            r#"
            [target.lib]
            kind = "cc_library"
            srcs = ["a.c"]
            shard_count = 2
            "#,
        )
        .unwrap_err();
        // `{:#}` so the assertion reads the cause, not just the "invalid
        // target" context wrapped around it.
        let error = format!("{error:#}");
        assert!(error.contains("test and cc_test targets only"), "{error}");

        let error = Manifest::parse_str(&sharded_test_manifest("shard_count = 0", "")).unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("at least 1"), "{error}");

        // Setting a shard variable by hand means something different from
        // sharding; overriding either direction would make one of them a lie.
        // Only a direct test can carry `env`, so the collision is tested there.
        let error = test_actions(
            r#"
            [toolchain.tools]
            runner = "runner"

            [target.split]
            kind = "test"
            tool = "runner"
            inputs = ["cases.txt"]
            shard_count = 2
            env = { TEST_SHARD_INDEX = "0" }
            "#,
        )
        .unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("shard_count"), "{error}");
        assert!(error.contains("TEST_SHARD_INDEX"), "{error}");

        // The same collision through pass_env, which names a host variable
        // rather than setting one.
        let error = test_actions(
            r#"
            [toolchain.tools]
            runner = "runner"

            [target.split]
            kind = "test"
            tool = "runner"
            inputs = ["cases.txt"]
            shard_count = 2
            pass_env = ["TEST_TOTAL_SHARDS"]
            "#,
        )
        .unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("TEST_TOTAL_SHARDS"), "{error}");
    }

    #[test]
    fn command_line_test_options_reach_every_test_action() {
        let manifest = r#"
            [toolchain.tools]
            runner = "runner"

            [target.t]
            kind = "test"
            tool = "runner"
            inputs = ["cases.txt"]
            args = ["--quiet"]
            env = { LEVEL = "manifest" }
            "#;
        let apply = |options: &TestOptions| {
            let mut graph =
                BuildGraph::from_manifest(&Manifest::parse_str(manifest).unwrap()).unwrap();
            graph.apply_test_options(options);
            let action = graph
                .actions
                .iter()
                .find(|action| action.kind == ActionKind::Test)
                .expect("test action");
            (action.argv.clone(), action.env.clone())
        };

        // Nothing supplied leaves the action exactly as the manifest wrote it.
        let (argv, env) = apply(&TestOptions::default());
        assert_eq!(argv, vec!["runner", "--quiet"]);
        assert_eq!(env["LEVEL"], "manifest");

        // A filter travels as the environment protocol runners already
        // implement, under both spellings, because Frost cannot know a
        // runner's filter flag.
        let (_, env) = apply(&TestOptions {
            filter: Some("parse::*".into()),
            ..Default::default()
        });
        assert_eq!(env["TESTBRIDGE_TEST_ONLY"], "parse::*");
        assert_eq!(env["GTEST_FILTER"], "parse::*");

        // Extra argv is appended, so the manifest's own arguments keep their
        // order and meaning.
        let (argv, _) = apply(&TestOptions {
            args: vec!["--verbose".into()],
            ..Default::default()
        });
        assert_eq!(argv, vec!["runner", "--quiet", "--verbose"]);

        // The command line wins over the manifest: it is the person typing
        // now, and the override lands in the key rather than passing silently.
        let (_, env) = apply(&TestOptions {
            env: vec![("LEVEL".into(), "cli".into())],
            ..Default::default()
        });
        assert_eq!(env["LEVEL"], "cli");
    }

    #[test]
    fn a_filtered_run_cannot_be_served_an_unfiltered_result() {
        // The property #142 asked for. Nothing new enters the action key to
        // get it: argv and env are already key material, so the filtered
        // action simply is a different action.
        let manifest = Manifest::parse_str(
            r#"
            [toolchain.tools]
            runner = "runner"

            [target.t]
            kind = "test"
            tool = "runner"
            inputs = ["cases.txt"]
            "#,
        )
        .unwrap();
        let plain = BuildGraph::from_manifest(&manifest).unwrap();
        let mut filtered = BuildGraph::from_manifest(&manifest).unwrap();
        filtered.apply_test_options(&TestOptions {
            filter: Some("only_this".into()),
            ..Default::default()
        });

        let test_of = |graph: &BuildGraph| {
            let action = graph
                .actions
                .iter()
                .find(|action| action.kind == ActionKind::Test)
                .expect("test action");
            (action.argv.clone(), action.env.clone())
        };
        assert_ne!(test_of(&plain), test_of(&filtered));
        // Same action id and stamp, though: it is the same test, asked a
        // narrower question. The key separates them through the environment.
        let id = |graph: &BuildGraph| {
            graph
                .actions
                .iter()
                .find(|action| action.kind == ActionKind::Test)
                .unwrap()
                .id
                .clone()
        };
        assert_eq!(id(&plain), id(&filtered));
    }

    #[test]
    fn a_value_given_on_the_command_line_stops_being_inherited() {
        // `pass_env` puts the host's value in the key. Once the command line
        // sets the same name, leaving it there would key on a value that no
        // longer applies.
        let manifest = Manifest::parse_str(
            r#"
            [toolchain.tools]
            runner = "runner"

            [target.t]
            kind = "test"
            tool = "runner"
            inputs = ["cases.txt"]
            pass_env = ["RUST_LOG"]
            "#,
        )
        .unwrap();
        let mut graph = BuildGraph::from_manifest(&manifest).unwrap();
        graph.apply_test_options(&TestOptions {
            env: vec![("RUST_LOG".into(), "debug".into())],
            ..Default::default()
        });
        let action = graph
            .actions
            .iter()
            .find(|action| action.kind == ActionKind::Test)
            .unwrap();
        assert_eq!(action.env["RUST_LOG"], "debug");
        assert!(
            action.pass_env.is_empty(),
            "an overridden name must not also be inherited: {:?}",
            action.pass_env
        );
    }

    #[test]
    fn flaky_retries_is_declared_where_it_can_mean_something() {
        let actions = test_actions(
            r#"
            [toolchain.tools]
            runner = "runner"

            [target.sometimes]
            kind = "test"
            tool = "runner"
            inputs = ["cases.txt"]
            flaky_retries = 2
            "#,
        )
        .unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].flaky_retries, 2);

        // Absent means one attempt, and every non-test action stays at zero:
        // retrying a compile that failed is a different and much worse idea.
        let actions = test_actions(
            r#"
            [toolchain.tools]
            runner = "runner"

            [target.plain]
            kind = "test"
            tool = "runner"
            inputs = ["cases.txt"]
            "#,
        )
        .unwrap();
        assert_eq!(actions[0].flaky_retries, 0);

        let error = Manifest::parse_str(
            r#"
            [target.lib]
            kind = "cc_library"
            srcs = ["a.c"]
            flaky_retries = 2
            "#,
        )
        .unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("test and cc_test targets only"), "{error}");

        // A ceiling, so a broken test cannot be made to look green by asking
        // for enough attempts.
        let error = Manifest::parse_str(
            r#"
            [toolchain.tools]
            runner = "runner"

            [target.desperate]
            kind = "test"
            tool = "runner"
            inputs = ["cases.txt"]
            flaky_retries = 50
            "#,
        )
        .unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("at most 9"), "{error}");
    }

    #[test]
    fn a_stamp_reference_is_classified_by_its_name_alone() {
        // The point of splitting by name: no subprocess has run, no value
        // exists yet, and the graph already knows which half each reference is
        // in. That is what lets a manifest be validated at load, and what keeps
        // the graph a pure function of the manifest — and therefore cacheable.
        let manifest = Manifest::parse_str(
            r#"
            [toolchain.tools]
            sh = "sh"

            [stamp]
            command = ["tools/status"]

            [target.release]
            kind = "command"
            tool = "sh"
            args = ["-c", "echo ${stamp.STABLE_V} ${stamp.BUILD_TIME} > ${out}"]
            env = { NOTE = "built at ${stamp.BUILD_TIME}" }
            inputs = []
            outputs = ["gen/${config}/v.txt"]
            "#,
        )
        .unwrap();
        let graph = BuildGraph::from_manifest(&manifest).unwrap();
        let action = graph
            .actions
            .iter()
            .find(|action| action.id == "command:release")
            .unwrap();
        assert_eq!(action.stable_stamps, ["STABLE_V"]);
        // Once, though it is referenced from both an argument and an
        // environment value: the pair describes the action, not how many times
        // the manifest happened to spell the name.
        assert_eq!(action.volatile_stamps, ["BUILD_TIME"]);
        // And the reference survives into the argv. Substituting the value here
        // would put a volatile value into the action key by the back door.
        assert!(
            action
                .argv
                .iter()
                .any(|arg| arg.contains("${stamp.BUILD_TIME}")),
            "{:?}",
            action.argv
        );
    }

    #[test]
    fn a_stamp_reference_without_a_stamp_section_is_a_manifest_error() {
        let error = BuildGraph::from_manifest(
            &Manifest::parse_str(
                r#"
                [toolchain.tools]
                sh = "sh"

                [target.release]
                kind = "command"
                tool = "sh"
                args = ["-c", "echo ${stamp.STABLE_V} > ${out}"]
                inputs = []
                outputs = ["gen/${config}/v.txt"]
                "#,
            )
            .unwrap(),
        )
        .unwrap_err()
        .to_string();
        // Not "unknown command variable": the name is spelled correctly and the
        // thing that is missing is somewhere else in the file.
        assert!(error.contains("[stamp] section"), "{error}");
        assert!(error.contains("STABLE_V"), "{error}");
    }

    #[test]
    fn an_empty_stable_prefix_makes_every_value_stable() {
        // A workspace declaring it has no volatile values, and would like to be
        // told if it ever adds one.
        let manifest = Manifest::parse_str(
            r#"
            [toolchain.tools]
            sh = "sh"

            [stamp]
            command = ["tools/status"]
            stable_prefix = ""

            [target.release]
            kind = "command"
            tool = "sh"
            args = ["-c", "echo ${stamp.BUILD_TIME} > ${out}"]
            inputs = []
            outputs = ["gen/${config}/v.txt"]
            "#,
        )
        .unwrap();
        let graph = BuildGraph::from_manifest(&manifest).unwrap();
        let action = graph
            .actions
            .iter()
            .find(|action| action.id == "command:release")
            .unwrap();
        assert_eq!(action.stable_stamps, ["BUILD_TIME"]);
        assert!(action.volatile_stamps.is_empty());
    }

    #[test]
    fn retry_policy_is_not_action_key_material() {
        // Turning retries on must not invalidate a result that already passed
        // cleanly: the policy says how hard to look for a verdict, not what
        // the test does. The action id, argv, env and stamp are what the key
        // is built from, so this pins that none of them moved.
        let plain = test_actions(
            r#"
            [toolchain.tools]
            runner = "runner"

            [target.t]
            kind = "test"
            tool = "runner"
            inputs = ["cases.txt"]
            "#,
        )
        .unwrap();
        let retried = test_actions(
            r#"
            [toolchain.tools]
            runner = "runner"

            [target.t]
            kind = "test"
            tool = "runner"
            inputs = ["cases.txt"]
            flaky_retries = 3
            "#,
        )
        .unwrap();
        assert_eq!(plain.len(), retried.len());
        assert_eq!(plain[0].id, retried[0].id);
        assert_eq!(plain[0].argv, retried[0].argv);
        assert_eq!(plain[0].env, retried[0].env);
        assert_eq!(plain[0].stamp, retried[0].stamp);
    }

    #[test]
    fn dep_references_name_one_dependency_without_repeating_its_layout() {
        // The whole point: the referencing target never writes gen/one.txt.
        let argv = user_argv(r#"["--single", "${dep:one}"]"#).unwrap();
        assert_eq!(argv, vec!["pack", "--single", "gen/one.txt"]);

        // A reference composes inside a larger argument, which is what a
        // `-Dkey=path` style flag needs.
        let argv = user_argv(r#"["--flag=${dep:one}"]"#).unwrap();
        assert_eq!(argv, vec!["pack", "--flag=gen/one.txt"]);

        // Several references in one argument each resolve.
        let argv = user_argv(r#"["${dep:one}:${dep:one}"]"#).unwrap();
        assert_eq!(argv, vec!["pack", "gen/one.txt:gen/one.txt"]);

        // The plural form takes every output of one dependency, as separate
        // argv items, and must occupy a whole argument like the other
        // multi-value variables.
        let argv = user_argv(r#"["--many", "${deps:two}"]"#).unwrap();
        assert_eq!(
            argv,
            vec!["pack", "--many", "gen/two-a.txt", "gen/two-b.txt"]
        );
        let error = user_argv(r#"["--many=${deps:two}"]"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("one complete argument"), "{error}");
    }

    #[test]
    fn dep_references_reject_what_they_cannot_mean() {
        // Not a declared dependency: resolving it would let the argv name a
        // file this target has no edge to, so the build could run before it
        // existed.
        let error = user_argv(r#"["${dep:absent}"]"#).unwrap_err().to_string();
        assert!(error.contains("not a declared dependency"), "{error}");
        assert!(error.contains("one, two, tree"), "{error}");

        // Several outputs have no single path this could mean. First-wins
        // would be silently wrong, so it names the plural form instead.
        let error = user_argv(r#"["${dep:two}"]"#).unwrap_err().to_string();
        assert!(error.contains("declares 2 outputs"), "{error}");
        assert!(error.contains("${deps:two}"), "{error}");

        // A target that owns only a directory has no file output to
        // substitute, and its tree stamp is Frost's bookkeeping rather than
        // a path to hand to a tool.
        let error = user_argv(r#"["${dep:tree}"]"#).unwrap_err().to_string();
        assert!(error.contains("declares no file outputs"), "{error}");
        let error = user_argv(r#"["${deps:tree}"]"#).unwrap_err().to_string();
        assert!(error.contains("declares no file outputs"), "{error}");

        let error = user_argv(r#"["${dep:one"]"#).unwrap_err().to_string();
        assert!(error.contains("unterminated"), "{error}");
    }

    #[test]
    fn a_dependency_that_moves_its_output_changes_the_referencing_argv() {
        // The expansion lands in argv, and argv is action-key material, so the
        // dependent rebuilds rather than replaying a command naming a path
        // that no longer exists. That is the property that makes the
        // indirection safe to rely on.
        let before = user_argv(r#"["${dep:one}"]"#).unwrap();
        let moved = dep_reference_manifest(r#"["${dep:one}"]"#)
            .replace("gen/one.txt", "gen/renamed/one.txt");
        let manifest = Manifest::parse_str(&moved).unwrap();
        let graph = BuildGraph::from_manifest(&manifest).unwrap();
        let after = graph
            .actions
            .iter()
            .find(|action| action.target == "user")
            .expect("user action")
            .argv
            .clone();
        assert_eq!(before, vec!["pack", "gen/one.txt"]);
        assert_eq!(after, vec!["pack", "gen/renamed/one.txt"]);
        assert_ne!(before, after);
    }

    /// The `user` target's manifest with `env_toml` added to it.
    fn env_manifest(env_toml: &str) -> String {
        format!(
            "{}\n            env = {env_toml}\n",
            dep_reference_manifest(r#"["--out", "${out}"]"#).trim_end()
        )
    }

    /// The `user` target's env, with `env_toml` spliced in.
    fn user_env(env_toml: &str) -> Result<BTreeMap<String, String>> {
        let manifest = Manifest::parse_str(&env_manifest(env_toml))?;
        let graph = BuildGraph::from_manifest(&manifest)?;
        Ok(graph
            .actions
            .iter()
            .find(|action| action.target == "user")
            .expect("user action")
            .env
            .clone())
    }

    #[test]
    fn dep_references_resolve_in_env_values() {
        // The case this exists for: a tool configured by environment rather
        // than by flags still should not have to write out where its
        // dependency puts things.
        let env = user_env(r#"{ ONE = "${dep:one}" }"#).unwrap();
        assert_eq!(env["ONE"], "gen/one.txt");

        // And composed, the way a `-D`-style value or a URL would be.
        let env = user_env(r#"{ ONE = "path=${dep:one};" }"#).unwrap();
        assert_eq!(env["ONE"], "path=gen/one.txt;");

        // A value with nothing addressed to Frost survives byte for byte. An
        // env value is handed to another program, and `${...}` in one is
        // routinely that program's own syntax rather than a typo.
        let env = user_env(r#"{ SHELLY = "${HOME}/x", LITERAL = "$notavar" }"#).unwrap();
        assert_eq!(env["SHELLY"], "${HOME}/x");
        assert_eq!(env["LITERAL"], "$notavar");
    }

    #[test]
    fn env_refuses_the_plural_form_rather_than_choosing_a_separator() {
        // `${deps:two}` is two paths. As argv that is two arguments; as one
        // environment variable it is a joiner Frost would have to invent, and
        // `:` versus `;` versus a space is exactly the platform-specific
        // guess this feature is meant to remove.
        let error = user_env(r#"{ MANY = "${deps:two}" }"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("one string"), "{error}");
        assert!(error.contains("${dep:LABEL}"), "{error}");

        // The other mistakes still report, and now say which env value.
        let error = user_env(r#"{ NOPE = "${dep:absent}" }"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("not a declared dependency"), "{error}");
        assert!(error.contains(r#"env "NOPE""#), "{error}");

        let error = user_env(r#"{ AMBIGUOUS = "${dep:two}" }"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("declares 2 outputs"), "{error}");
    }

    #[test]
    fn a_dependency_that_moves_its_output_changes_the_referencing_env() {
        // Same guarantee as argv: env is action-key material, so a dependency
        // that relocates its output reruns the consumer instead of replaying a
        // command configured with a path that no longer exists.
        let before = user_env(r#"{ ONE = "${dep:one}" }"#).unwrap();
        let moved = Manifest::parse_str(
            &env_manifest(r#"{ ONE = "${dep:one}" }"#)
                .replace("gen/one.txt", "gen/renamed/one.txt"),
        )
        .unwrap();
        let after = BuildGraph::from_manifest(&moved)
            .unwrap()
            .actions
            .iter()
            .find(|action| action.target == "user")
            .expect("user action")
            .env
            .clone();
        assert_eq!(before["ONE"], "gen/one.txt");
        assert_eq!(after["ONE"], "gen/renamed/one.txt");
    }

    #[test]
    fn a_genrule_cmd_can_name_a_dependency_instead_of_its_layout() {
        let cmd = |script: &str| -> Result<String> {
            let manifest = Manifest::parse_str(&format!(
                r#"
                [target.one]
                kind = "genrule"
                cmd = "sh one.sh ${{out}}"
                inputs = ["one.sh"]
                outputs = ["gen/one.txt"]

                [target.two]
                kind = "genrule"
                cmd = "sh two.sh ${{outs}}"
                inputs = ["two.sh"]
                outputs = ["gen/two-a.txt", "gen/two-b.txt"]

                [target.bundle]
                kind = "genrule"
                cmd = "{script}"
                inputs = ["bundle.sh"]
                outputs = ["gen/bundle.txt"]
                deps = ["one", "two"]
                "#
            ))?;
            let graph = BuildGraph::from_manifest(&manifest)?;
            let action = graph
                .actions
                .iter()
                .find(|action| action.target == "bundle")
                .expect("bundle action");
            // argv is [shell, flag, script]; the script is what was expanded.
            Ok(action.argv.last().expect("script").clone())
        };

        assert_eq!(
            cmd("sh bundle.sh ${dep:one} -o ${out}").unwrap(),
            "sh bundle.sh gen/one.txt -o gen/bundle.txt"
        );

        // A genrule cmd is one shell string, so the plural form joins on a
        // space -- the separator `${in}` and `${outs}` already use here. That
        // convention is the shell's, which is why `env` refuses the same form
        // rather than borrowing a separator it has no basis for.
        assert_eq!(
            cmd("cat ${deps:two} > ${out}").unwrap(),
            "cat gen/two-a.txt gen/two-b.txt > gen/bundle.txt"
        );

        // The undeclared and ambiguous cases report against the cmd, not
        // against a "command arg" the author never wrote.
        let error = cmd("cp ${dep:absent} ${out}").unwrap_err().to_string();
        assert!(error.contains("not a declared dependency"), "{error}");
        assert!(error.contains("genrule cmd"), "{error}");

        let error = cmd("cp ${dep:two} ${out}").unwrap_err().to_string();
        assert!(error.contains("declares 2 outputs"), "{error}");

        // An unknown variable still names the supported set, now including
        // the two reference forms.
        let error = cmd("cp ${nonsense} ${out}").unwrap_err().to_string();
        assert!(error.contains("${dep:LABEL}"), "{error}");
    }

    #[test]
    fn allpaths_walks_every_route_and_bounds_itself() {
        // A diamond: `top` reaches `bottom` through both `left` and `right`,
        // which is exactly what one path cannot describe.
        let manifest = Manifest::parse_str(
            r#"
            [target.bottom]
            kind = "cc_library"
            srcs = ["bottom.c"]

            [target.left]
            kind = "cc_library"
            srcs = ["left.c"]
            deps = ["bottom"]

            [target.right]
            kind = "cc_library"
            srcs = ["right.c"]
            deps = ["bottom"]

            [target.top]
            kind = "cc_binary"
            srcs = ["top.c"]
            deps = ["left", "right"]
            "#,
        )
        .unwrap();
        let graph = BuildGraph::from_manifest(&manifest).unwrap();

        let found = graph.allpaths("top", "bottom", 16).unwrap();
        assert!(!found.truncated);
        assert_eq!(
            found.paths,
            vec![
                vec!["top".to_string(), "left".to_string(), "bottom".to_string()],
                vec!["top".to_string(), "right".to_string(), "bottom".to_string()],
            ]
        );

        // Reaching a target twice by different routes must not be mistaken for
        // a cycle: a single global visited set would report only one path.
        assert_eq!(graph.allpaths("top", "left", 16).unwrap().paths.len(), 1);

        // Direction is not symmetric, and no route is an empty answer rather
        // than an error.
        let none = graph.allpaths("bottom", "top", 16).unwrap();
        assert!(none.paths.is_empty() && !none.truncated);

        // The bound stops the walk and says so.
        let capped = graph.allpaths("top", "bottom", 1).unwrap();
        assert!(capped.truncated);
        assert_eq!(capped.paths.len(), 1);

        assert!(graph.allpaths("top", "nope", 16).is_err());
    }

    #[test]
    fn owners_reports_targets_that_declare_a_file() {
        let graph = BuildGraph::from_manifest(&demo_manifest()).unwrap();

        assert_eq!(
            graph.owners(&["src/util.c".to_string()]).unwrap(),
            vec!["util".to_string()]
        );

        // The generated header belongs to the target that compiles against it,
        // not to the genrule that writes it.
        assert_eq!(
            graph.owners(&["gen/config.h".to_string()]).unwrap(),
            vec!["app".to_string()]
        );

        // A pattern nobody declares is empty rather than an error: asking
        // about a file that turns out not to be an input is a normal question.
        assert!(graph.owners(&["nope.c".to_string()]).unwrap().is_empty());

        // `*` stops at `/`, so a pattern rooted at the workspace cannot reach
        // into a directory; `**` is how a caller asks for the whole tree.
        assert!(graph.owners(&["*.c".to_string()]).unwrap().is_empty());
        assert_eq!(
            graph.owners(&["**/*.c".to_string()]).unwrap(),
            vec!["app".to_string(), "util".to_string()]
        );

        assert!(graph.owners(&["[".to_string()]).is_err());
    }

    #[test]
    fn platform_isolates_paths_and_selects_toolchain() {
        let manifest = Manifest::parse_str(
            r#"
            [toolchain]
            cc = "cc"

            [platform.cross]
            cc = "cross-gcc"
            ar = "cross-ar"
            arflags = ["rcs"]

            [target.util]
            kind = "cc_library"
            srcs = ["src/util.c"]

            [target.app]
            kind = "cc_binary"
            srcs = ["src/main.c"]
            deps = ["util"]
            "#,
        )
        .unwrap();
        let graph = BuildGraph::from_manifest_configured(&manifest, "debug", "cross").unwrap();
        assert_eq!(graph.platform, "cross");

        let compile = graph
            .actions
            .iter()
            .find(|a| a.id == "compile:app:src/main.c")
            .unwrap();
        assert_eq!(compile.argv[0], "cross-gcc");
        let obj = format!("{OBJ_DIR}/cross/debug/app/src/main.c.o");
        assert!(compile.argv.contains(&obj), "argv: {:?}", compile.argv);

        let archive = graph
            .actions
            .iter()
            .find(|a| a.id == "archive:util")
            .unwrap();
        assert_eq!(archive.argv[0], "cross-ar");
        assert_eq!(archive.argv[1], "rcs");
        assert!(archive
            .argv
            .contains(&format!("{LIB_DIR}/cross/debug/libutil.a")));

        let link = graph.actions.iter().find(|a| a.id == "link:app").unwrap();
        assert!(link.argv.contains(&format!(
            "{BIN_DIR}/cross/debug/app{}",
            std::env::consts::EXE_SUFFIX
        )));

        // The host graph keeps historical platform-free paths.
        let host = BuildGraph::from_manifest_with_profile(&manifest, "debug").unwrap();
        let host_link = host.actions.iter().find(|a| a.id == "link:app").unwrap();
        assert!(host_link.argv.contains(&format!(
            "{BIN_DIR}/debug/app{}",
            std::env::consts::EXE_SUFFIX
        )));
    }

    #[test]
    fn platform_selects_kofun_compiler_and_output_tree() {
        let manifest = Manifest::parse_str(
            r#"
            [toolchain]
            kofunc = "host-kofun"

            [platform.device]
            kofunc = "device-kofun"

            [target.app]
            kind = "kofun_binary"
            srcs = ["main.kofun"]
            "#,
        )
        .unwrap();
        let graph = BuildGraph::from_manifest_configured(&manifest, "release", "device").unwrap();
        let action = graph.actions.iter().find(|a| a.id == "kofun:app").unwrap();
        assert_eq!(action.argv[0], "device-kofun");
        assert!(action.argv.contains(&format!(
            "{BIN_DIR}/device/release/app{}",
            std::env::consts::EXE_SUFFIX
        )));
        assert!(action
            .argv
            .contains(&format!("{OBJ_DIR}/device/release/app/kofun.c")));
    }

    #[test]
    fn an_undeclared_profile_is_a_typo_not_a_silent_new_tree() {
        let manifest = Manifest::parse_str(
            r#"
            [profile.release]
            cflags = ["-O2"]

            [target.app]
            kind = "cc_binary"
            srcs = ["a.c"]
            "#,
        )
        .unwrap();
        let err = BuildGraph::from_manifest_with_profile(&manifest, "relase")
            .unwrap_err()
            .to_string();
        assert!(err.contains("did you mean"), "{err}");
        assert!(err.contains("release"), "{err}");

        // debug always works, declared or not: it is the default.
        assert!(BuildGraph::from_manifest_with_profile(&manifest, "debug").is_ok());
        assert!(BuildGraph::from_manifest_with_profile(&manifest, "release").is_ok());

        // A workspace that declares no profiles keeps naming trees freely.
        let bare = Manifest::parse_str("[target.app]\nkind='cc_binary'\nsrcs=['a.c']\n").unwrap();
        assert!(BuildGraph::from_manifest_with_profile(&bare, "scratch").is_ok());
    }

    #[test]
    fn detects_dependency_cycle() {
        let manifest = Manifest::parse_str(
            r#"
            [target.a]
            kind = "cc_library"
            srcs = ["a.c"]
            deps = ["b"]

            [target.b]
            kind = "cc_library"
            srcs = ["b.c"]
            deps = ["a"]
            "#,
        )
        .unwrap();
        let err = BuildGraph::from_manifest(&manifest)
            .unwrap_err()
            .to_string();
        assert!(err.contains("dependency cycle"), "{err}");
    }

    #[test]
    fn rejects_duplicate_outputs() {
        let manifest = Manifest::parse_str(
            r#"
            [target.g1]
            kind = "genrule"
            cmd = "true"
            outputs = ["gen/same.h"]

            [target.g2]
            kind = "genrule"
            cmd = "true"
            outputs = ["gen/same.h"]
            "#,
        )
        .unwrap();
        let err = BuildGraph::from_manifest(&manifest)
            .unwrap_err()
            .to_string();
        assert!(err.contains("produced by both"), "{err}");
    }

    #[test]
    fn unknown_target_in_closure_errors() {
        let graph = BuildGraph::from_manifest(&demo_manifest()).unwrap();
        assert!(graph.action_closure(&["nope".to_string()]).is_err());
    }
}
