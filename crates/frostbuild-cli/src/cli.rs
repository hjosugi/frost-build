//! Every flag, subcommand and value type `frost` accepts, and nothing else.
//!
//! The command surface is a compatibility promise (`docs/28_compatibility_contract.md`)
//! checked against `tests/cli-surface.txt`, so it is worth being able to read
//! the whole of it at once. Behaviour lives in the module named after the
//! command; what is here is the shape of the argument, its help text, and how
//! it completes.

use std::path::PathBuf;

use clap::Parser;
use clap::Subcommand;
use clap::ValueEnum;
use clap::ValueHint;
use clap_complete::ArgValueCompleter;

use crate::completions::{
    complete_attr_filter, complete_config, complete_info_key, complete_npm_script,
    complete_platform, complete_profile, complete_remote_cache, complete_target,
    complete_target_kind, complete_test_target,
};

#[derive(Parser)]
#[command(
    name = "frost",
    version,
    about = "frostbuild: correct, fast incremental builds",
    // A later occurrence of an option wins instead of being an error. This is
    // what lets `.frostrc` defaults be injected ahead of the real command line
    // and get the documented precedence for free, rather than reimplementing
    // clap's parsing and type checking to merge two sources by hand.
    args_override_self = true
)]
pub(crate) struct Cli {
    /// Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)
    #[arg(
        short = 'C',
        long = "workspace",
        default_value = ".",
        global = true,
        value_hint = ValueHint::DirPath
    )]
    pub(crate) workspace: PathBuf,

    /// Apply a named `[config.NAME]` section from `.frostrc`. Repeatable;
    /// applied in the order given
    #[arg(
        long,
        value_name = "NAME",
        global = true,
        add = ArgValueCompleter::new(complete_config)
    )]
    pub(crate) config: Vec<String>,

    /// Ignore `.frostrc` entirely, so only the command line and built-in
    /// defaults apply
    #[arg(long, global = true)]
    pub(crate) no_frostrc: bool,

    /// Write one JSON object per line describing the build, for CI and
    /// dashboards. Independent of the terminal output
    #[arg(
        long,
        value_name = "FILE",
        global = true,
        value_hint = ValueHint::FilePath
    )]
    pub(crate) build_event_json: Option<PathBuf>,

    #[command(subcommand)]
    pub(crate) command: Cmd,
}

#[derive(Subcommand)]
pub(crate) enum Cmd {
    /// Build targets (default: workspace default_targets)
    Build {
        #[arg(add = ArgValueCompleter::new(complete_target))]
        targets: Vec<String>,
        /// Number of parallel jobs (default: number of CPUs)
        #[arg(short = 'j', long)]
        jobs: Option<usize>,
        /// Keep building independent actions after a failure
        #[arg(short = 'k', long)]
        keep_going: bool,
        /// After the build, print why each action ran or was cached
        #[arg(long)]
        explain: bool,
        /// Print full command lines as they run
        #[arg(short, long)]
        verbose: bool,
        /// Build profile; outputs and caches are isolated per profile
        #[arg(
            long,
            default_value = "debug",
            add = ArgValueCompleter::new(complete_profile)
        )]
        profile: String,
        /// Target platform from [platform.<name>] for cross/device builds;
        /// outputs and caches are isolated per platform
        #[arg(
            long,
            default_value = frostbuild_core::manifest::HOST_PLATFORM,
            add = ArgValueCompleter::new(complete_platform),
            conflicts_with = "all_platforms"
        )]
        platform: String,
        /// Build host and every declared [platform.*] configuration
        #[arg(long)]
        all_platforms: bool,
        /// Disable successful test-result cache
        #[arg(long)]
        no_cache: bool,
        /// Skip the workspace's [stamp] command. Every ${stamp.KEY} then
        /// expands to nothing, which changes the action key of anything that
        /// reads a stable value — a stamp-free build is a different build, and
        /// says so rather than reusing results that embedded a value
        #[arg(long)]
        no_stamp: bool,
        /// A failing [stamp] command leaves the values empty instead of
        /// failing the build. Off by default: a status script that stopped
        /// working should be noticed, not silently ship a binary with no
        /// version in it
        #[arg(long, conflicts_with = "no_stamp")]
        stamp_optional: bool,
        /// Shared cache consulted when the local journal misses: a directory
        /// path, file:///path, or http://host/prefix. Never required for
        /// correctness — every response is verified and any failure falls back
        /// to building locally
        #[arg(
            long,
            value_name = "ENDPOINT",
            add = ArgValueCompleter::new(complete_remote_cache)
        )]
        remote_cache: Option<String>,
        /// Also publish what this build produces to --remote-cache
        #[arg(long, requires = "remote_cache")]
        remote_upload: bool,
        /// Seconds to wait for one remote cache request
        #[arg(
            long,
            value_name = "SECONDS",
            default_value = "10",
            requires = "remote_cache"
        )]
        remote_timeout: u64,
        /// Isolate actions from undeclared workspace files with bubblewrap
        #[arg(long)]
        sandbox: bool,
        /// Execute each selected action twice and compare output digests
        #[arg(long, num_args = 0..=1, default_missing_value = "0", require_equals = true)]
        check_determinism: Option<Option<usize>>,
        /// Stop any action still running after this many seconds. A target's
        /// own `timeout` wins; tests carry a default limit without this flag
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<u64>,
        /// Write a Chrome/Perfetto trace JSON
        #[arg(long, value_hint = ValueHint::FilePath)]
        trace: Option<PathBuf>,
        /// Write a self-contained HTML report of this build; `--report=PATH`
        /// chooses where, plain `--report` writes under .frost/report/
        #[arg(long, num_args = 0..=1, require_equals = true, value_name = "PATH", value_hint = ValueHint::FilePath)]
        report: Option<Option<PathBuf>>,
        /// Report scheduling measurements: makespan, worker utilization and
        /// distance from the estimated critical path
        #[arg(long)]
        stats: bool,
        /// Disable the interactive terminal UI and print plain progress lines
        #[arg(long)]
        no_tui: bool,
        /// Execute through the per-workspace frostd service
        #[arg(long)]
        daemon: bool,
        #[arg(long, value_enum, default_value = "critical-path")]
        scheduler: SchedulerArg,
        #[arg(long, value_enum, default_value = "journal")]
        estimator: EstimatorArg,
    },
    /// Build one target and execute its native or language artifact
    Run {
        #[arg(add = ArgValueCompleter::new(complete_target))]
        target: Option<String>,
        #[arg(short = 'j', long)]
        jobs: Option<usize>,
        #[arg(
            long,
            default_value = "debug",
            add = ArgValueCompleter::new(complete_profile)
        )]
        profile: String,
        #[arg(
            long,
            default_value = frostbuild_core::manifest::HOST_PLATFORM,
            add = ArgValueCompleter::new(complete_platform)
        )]
        platform: String,
        /// Explicit executable prefix for cross/emulated or custom artifacts
        #[arg(long, value_hint = ValueHint::ExecutablePath)]
        runner: Option<PathBuf>,
        /// Print the exact direct argv without executing it
        #[arg(long)]
        print: bool,
        /// Arguments passed to the built program (after `--`)
        #[arg(last = true)]
        program_args: Vec<String>,
    },
    /// Rebuild on source/manifest changes and optionally restart a dev process
    Watch {
        #[arg(add = ArgValueCompleter::new(complete_target))]
        targets: Vec<String>,
        /// Number of parallel build jobs (default: number of CPUs)
        #[arg(short = 'j', long)]
        jobs: Option<usize>,
        #[arg(
            long,
            default_value = "debug",
            add = ArgValueCompleter::new(complete_profile)
        )]
        profile: String,
        #[arg(
            long,
            default_value = frostbuild_core::manifest::HOST_PLATFORM,
            add = ArgValueCompleter::new(complete_platform)
        )]
        platform: String,
        /// Quiet period used to coalesce editor save events
        #[arg(long, default_value_t = 50)]
        debounce_ms: u64,
        /// Direct argv to start after a successful build and restart on success;
        /// place this option last when its arguments begin with '-'
        #[arg(long, num_args = 1.., allow_hyphen_values = true)]
        run: Vec<String>,
    },
    /// Watch one runnable target and restart its inferred artifact on success
    Dev {
        #[arg(add = ArgValueCompleter::new(complete_target))]
        target: Option<String>,
        #[arg(short = 'j', long)]
        jobs: Option<usize>,
        #[arg(
            long,
            default_value = "debug",
            add = ArgValueCompleter::new(complete_profile)
        )]
        profile: String,
        #[arg(
            long,
            default_value = frostbuild_core::manifest::HOST_PLATFORM,
            add = ArgValueCompleter::new(complete_platform)
        )]
        platform: String,
        #[arg(long, default_value_t = 50)]
        debounce_ms: u64,
        /// Explicit executable prefix for cross/emulated or custom artifacts
        #[arg(long, value_hint = ValueHint::ExecutablePath)]
        runner: Option<PathBuf>,
        /// Arguments passed to the restarted program (after `--`)
        #[arg(last = true)]
        program_args: Vec<String>,
    },
    /// Build one target and launch its native or language debugger
    Debug {
        #[arg(add = ArgValueCompleter::new(complete_target))]
        target: Option<String>,
        #[arg(short = 'j', long)]
        jobs: Option<usize>,
        #[arg(
            long,
            default_value = "debug",
            add = ArgValueCompleter::new(complete_profile)
        )]
        profile: String,
        #[arg(
            long,
            default_value = frostbuild_core::manifest::HOST_PLATFORM,
            add = ArgValueCompleter::new(complete_platform)
        )]
        platform: String,
        /// Debugger/runtime executable, or auto for GDB/LLDB, jdb, Node or pdb
        #[arg(long, default_value = "auto", value_hint = ValueHint::ExecutablePath)]
        debugger: String,
        /// Print the exact debugger argv without launching it
        #[arg(long)]
        print: bool,
        /// Arguments passed to the program being debugged (after `--`)
        #[arg(last = true)]
        program_args: Vec<String>,
    },
    /// Build one target and generate VS Code build/debug configuration
    Ide {
        #[arg(add = ArgValueCompleter::new(complete_target))]
        target: Option<String>,
        #[arg(short = 'j', long)]
        jobs: Option<usize>,
        #[arg(
            long,
            default_value = "debug",
            add = ArgValueCompleter::new(complete_profile)
        )]
        profile: String,
        #[arg(
            long,
            default_value = frostbuild_core::manifest::HOST_PLATFORM,
            add = ArgValueCompleter::new(complete_platform)
        )]
        platform: String,
        /// Workspace-relative VS Code directory
        #[arg(long, default_value = ".vscode", value_hint = ValueHint::DirPath)]
        output: PathBuf,
        /// Print the generated file map without writing it
        #[arg(long)]
        dry_run: bool,
    },
    /// Diagnose workspace, required tools and optional developer integrations
    Doctor {
        #[arg(
            long,
            default_value = "debug",
            add = ArgValueCompleter::new(complete_profile)
        )]
        profile: String,
        #[arg(
            long,
            default_value = frostbuild_core::manifest::HOST_PLATFORM,
            add = ArgValueCompleter::new(complete_platform)
        )]
        platform: String,
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Build and run test/cc_test targets
    Test {
        #[arg(add = ArgValueCompleter::new(complete_test_target))]
        targets: Vec<String>,
        #[arg(short = 'j', long)]
        jobs: Option<usize>,
        #[arg(short = 'k', long)]
        keep_going: bool,
        #[arg(long)]
        affected: bool,
        #[arg(long)]
        predictive: bool,
        #[arg(long, conflicts_with_all = ["affected", "predictive"])]
        all: bool,
        #[arg(long)]
        no_cache: bool,
        /// Skip the workspace's [stamp] command. Every ${stamp.KEY} then
        /// expands to nothing, which changes the action key of anything that
        /// reads a stable value — a stamp-free build is a different build, and
        /// says so rather than reusing results that embedded a value
        #[arg(long)]
        no_stamp: bool,
        /// A failing [stamp] command leaves the values empty instead of
        /// failing the build. Off by default: a status script that stopped
        /// working should be noticed, not silently ship a binary with no
        /// version in it
        #[arg(long, conflicts_with = "no_stamp")]
        stamp_optional: bool,
        /// Run only cases matching this pattern. Passed to the runner through
        /// TESTBRIDGE_TEST_ONLY and GTEST_FILTER, and part of the action key,
        /// so a filtered run is a separate result rather than one that
        /// satisfies an unfiltered request
        #[arg(long, value_name = "PATTERN")]
        test_filter: Option<String>,
        /// Set an environment variable for every test, as KEY=VALUE. Overrides
        /// a manifest value of the same name, and participates in the action
        /// key
        #[arg(long, value_name = "KEY=VALUE")]
        test_env: Vec<String>,
        /// Append an argument to every test's command line. Participates in
        /// the action key. Hyphens are allowed, since a runner's own flags are
        /// the usual thing to pass here
        #[arg(long, value_name = "ARG", allow_hyphen_values = true)]
        test_arg: Vec<String>,
        /// Run every test this many times, requiring all runs to pass. Does not
        /// read the cache — a recorded single pass cannot answer whether a test
        /// passes repeatedly — and suppresses `flaky_retries`, which would
        /// otherwise hide the failures this is looking for
        #[arg(long, value_name = "N", default_value = "1")]
        runs_per_test: u32,
        /// How much test output to show: `summary` for the counts alone,
        /// `errors` for failing tests replayed after the run, `all` for
        /// everything including passing tests
        #[arg(long, value_enum, default_value = "errors")]
        test_output: TestOutputArg,
        /// Shared cache consulted when the local journal misses: a directory
        /// path, file:///path, or http://host/prefix. Never required for
        /// correctness — every response is verified and any failure falls back
        /// to building locally
        #[arg(
            long,
            value_name = "ENDPOINT",
            add = ArgValueCompleter::new(complete_remote_cache)
        )]
        remote_cache: Option<String>,
        /// Also publish what this build produces to --remote-cache
        #[arg(long, requires = "remote_cache")]
        remote_upload: bool,
        /// Seconds to wait for one remote cache request
        #[arg(
            long,
            value_name = "SECONDS",
            default_value = "10",
            requires = "remote_cache"
        )]
        remote_timeout: u64,
        #[arg(long)]
        explain: bool,
        /// Write a self-contained HTML report of this run; `--report=PATH`
        /// chooses where, plain `--report` writes under .frost/report/
        #[arg(long, num_args = 0..=1, require_equals = true, value_name = "PATH", value_hint = ValueHint::FilePath)]
        report: Option<Option<PathBuf>>,
        /// Stop any test still running after this many seconds; overrides the
        /// default limit and is itself overridden by a target's own `timeout`
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<u64>,
        #[arg(
            long,
            default_value = "debug",
            add = ArgValueCompleter::new(complete_profile)
        )]
        profile: String,
        #[arg(
            long,
            default_value = frostbuild_core::manifest::HOST_PLATFORM,
            add = ArgValueCompleter::new(complete_platform),
            conflicts_with = "all_platforms"
        )]
        platform: String,
        /// Test host and every declared [platform.*] configuration
        #[arg(long)]
        all_platforms: bool,
        #[arg(long)]
        sandbox: bool,
        /// Disable the interactive terminal UI and print plain progress lines
        #[arg(long)]
        no_tui: bool,
        #[arg(long)]
        daemon: bool,
        #[arg(long, value_enum, default_value = "critical-path")]
        scheduler: SchedulerArg,
        #[arg(long, value_enum, default_value = "journal")]
        estimator: EstimatorArg,
    },
    /// Show which actions would run and why, without executing anything
    Plan {
        #[arg(add = ArgValueCompleter::new(complete_target))]
        targets: Vec<String>,
        #[arg(
            long,
            default_value = "debug",
            add = ArgValueCompleter::new(complete_profile)
        )]
        profile: String,
        #[arg(
            long,
            default_value = frostbuild_core::manifest::HOST_PLATFORM,
            add = ArgValueCompleter::new(complete_platform)
        )]
        platform: String,
    },
    /// Remove build outputs (--cache also removes the journal and hash cache)
    Clean {
        #[arg(long)]
        cache: bool,
        #[arg(long, add = ArgValueCompleter::new(complete_profile))]
        profile: Option<String>,
        #[arg(long, add = ArgValueCompleter::new(complete_platform))]
        platform: Option<String>,
    },
    /// Print the target dependency graph
    Graph {
        /// Emit Graphviz dot instead of text
        #[arg(long)]
        dot: bool,
        #[arg(
            long,
            default_value = "debug",
            add = ArgValueCompleter::new(complete_profile)
        )]
        profile: String,
        #[arg(
            long,
            default_value = frostbuild_core::manifest::HOST_PLATFORM,
            add = ArgValueCompleter::new(complete_platform)
        )]
        platform: String,
    },
    /// Export JSON Compilation Database for clangd/IDE integrations
    Compdb {
        #[arg(
            long,
            default_value = "compile_commands.json",
            value_hint = ValueHint::FilePath
        )]
        output: PathBuf,
        #[arg(
            long,
            default_value = "debug",
            add = ArgValueCompleter::new(complete_profile)
        )]
        profile: String,
        #[arg(
            long,
            default_value = frostbuild_core::manifest::HOST_PLATFORM,
            add = ArgValueCompleter::new(complete_platform)
        )]
        platform: String,
    },
    /// Merge gcov coverage data into an lcov tracefile
    ///
    /// Reads every `.gcda` a run produced, pairs it with the `.gcno` the
    /// compile wrote, and emits lcov. frost emits the format itself because
    /// neither `lcov` nor `gcovr` ships with a toolchain, and delegating would
    /// put a Perl dependency in every CI image that wanted coverage.
    CoverageLcov {
        /// Directory holding the run's `.gcda` counter files
        #[arg(long, value_hint = ValueHint::DirPath)]
        gcda: PathBuf,
        /// Object tree holding the matching `.gcno` notes files
        #[arg(long, value_hint = ValueHint::DirPath)]
        objects: PathBuf,
        /// Where to write the tracefile
        #[arg(long, value_hint = ValueHint::FilePath)]
        output: PathBuf,
        /// gcov executable, when it is not the one on PATH
        #[arg(long, default_value = "gcov", value_hint = ValueHint::CommandName)]
        gcov: String,
    },
    /// Explain the most recently recorded decision for a target
    Explain {
        #[arg(add = ArgValueCompleter::new(complete_target))]
        target: String,
        #[arg(
            long,
            default_value = "debug",
            add = ArgValueCompleter::new(complete_profile)
        )]
        profile: String,
        #[arg(
            long,
            default_value = frostbuild_core::manifest::HOST_PLATFORM,
            add = ArgValueCompleter::new(complete_platform)
        )]
        platform: String,
    },
    /// Write a safe native C/C++ or Java starter frost.toml from sources here
    Init {
        /// Print the manifest instead of writing it
        #[arg(long)]
        dry_run: bool,
        /// Source family; omit to auto-detect (mixed families require a choice)
        #[arg(long, value_enum)]
        language: Option<InitLanguage>,
        /// Write only frostw, frostw.cmd and .frost-version, pinned to this
        /// frost, into a workspace that already has a manifest
        #[arg(long, conflicts_with = "language")]
        wrapper: bool,
    },
    /// Compare scheduling strategies without building anything
    Simulate {
        #[arg(add = ArgValueCompleter::new(complete_target))]
        targets: Vec<String>,
        /// Worker counts to sweep (default: 1,2,4,8,16 capped at this host)
        #[arg(long, value_delimiter = ',')]
        jobs: Option<Vec<usize>>,
        #[arg(
            long,
            default_value = "debug",
            add = ArgValueCompleter::new(complete_profile)
        )]
        profile: String,
        #[arg(
            long,
            default_value = frostbuild_core::manifest::HOST_PLATFORM,
            add = ArgValueCompleter::new(complete_platform)
        )]
        platform: String,
        #[arg(long)]
        json: bool,
    },
    /// Query the target dependency graph (configuration-free)
    Query {
        #[command(subcommand)]
        function: QueryCmd,
    },
    /// Inspect local content-addressed cache storage and chunk reuse
    Cache {
        #[command(subcommand)]
        command: CacheCmd,
    },
    /// Rewrite frost.toml in its canonical form
    Fmt {
        /// Report whether anything would change and exit non-zero if so,
        /// without writing. For CI
        #[arg(long)]
        check: bool,
    },
    /// Report manifest patterns that build but cost something later
    Lint {
        /// Emit findings as one machine-readable JSON object
        #[arg(long)]
        json: bool,
    },
    /// Explain why a build reused a result or did not
    Journal {
        #[command(subcommand)]
        command: JournalCmd,
    },
    /// Manage the per-workspace build daemon
    Daemon {
        #[command(subcommand)]
        command: DaemonCmd,
    },
    /// Convert the supported Ninja rule/build subset to frost.toml
    ImportNinja {
        #[arg(default_value = "build.ninja", value_hint = ValueHint::FilePath)]
        ninja: PathBuf,
        #[arg(long, default_value = "frost.toml", value_hint = ValueHint::FilePath)]
        output: PathBuf,
    },
    /// Import a conservative native C/C++ subset from Bazel query XML
    ImportBazel {
        /// Bazel query expression to import
        #[arg(long, default_value = "//...")]
        query: String,
        /// Bazel or Bazelisk executable (defaults to BAZEL_BIN, bazel, bazelisk)
        #[arg(long, value_hint = ValueHint::ExecutablePath)]
        bazel: Option<PathBuf>,
        /// Print every generated manifest without writing
        #[arg(long)]
        dry_run: bool,
    },
    /// Import npm workspace validation gates and explicit Vite build boundaries
    ImportNpm {
        /// Non-interactive validation script to import; repeat or comma-separate
        #[arg(
            long = "script",
            value_delimiter = ',',
            add = ArgValueCompleter::new(complete_npm_script)
        )]
        scripts: Vec<String>,
        /// Also import recognized `vite build` scripts with profile-specific dist trees
        #[arg(long)]
        vite_builds: bool,
        /// npm executable recorded as a fingerprinted named tool
        #[arg(long, default_value = "npm", value_hint = ValueHint::ExecutablePath)]
        npm: PathBuf,
        /// Node executable recorded with npm's toolchain closure
        #[arg(long, default_value = "node", value_hint = ValueHint::ExecutablePath)]
        node: PathBuf,
        /// Print the generated root manifest without writing it
        #[arg(long)]
        dry_run: bool,
    },
    /// Watch, incrementally build, and restart a Bazel runnable target
    BazelDev {
        /// Canonical Bazel runnable label, for example //app:server
        target: String,
        /// Bazel or Bazelisk executable (defaults to BAZEL_BIN, bazel, bazelisk)
        #[arg(long, value_hint = ValueHint::ExecutablePath)]
        bazel: Option<PathBuf>,
        /// Quiet period used to coalesce editor filesystem events
        #[arg(long, default_value_t = 50)]
        debounce_ms: u64,
        /// Build option forwarded to both `bazel build` and `bazel run`
        #[arg(long = "bazel-arg", allow_hyphen_values = true)]
        bazel_args: Vec<String>,
        /// Arguments passed to the target after `--`
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Pack a directory into a deterministic compressed Java archive
    PackJar {
        /// Workspace-relative directory whose contents become JAR entries
        #[arg(long, value_hint = ValueHint::DirPath)]
        input: PathBuf,
        /// Workspace-relative output JAR
        #[arg(long, value_hint = ValueHint::FilePath)]
        output: PathBuf,
        /// Optional Java binary name for the Main-Class manifest attribute
        #[arg(long)]
        main_class: Option<String>,
    },
    /// Pack a pure-Python source tree into a deterministic standards-compliant wheel
    PackWheel {
        /// Workspace-relative source root whose contents install into purelib
        #[arg(long, value_hint = ValueHint::DirPath)]
        input: PathBuf,
        /// Python distribution name written to wheel metadata
        #[arg(long)]
        distribution: String,
        /// Normalized numeric Python release version (for example 1.2.3)
        #[arg(long)]
        version: String,
        /// Workspace-relative output wheel (must use the standard wheel filename)
        #[arg(long, value_hint = ValueHint::FilePath)]
        output: PathBuf,
    },
    /// Speak the Language Server Protocol for frost.toml on stdin/stdout
    Lsp,
    /// Report workspace, output and cache locations for scripts and editors
    Info {
        /// Print only this key's value; omit for the whole table
        #[arg(add = ArgValueCompleter::new(complete_info_key))]
        key: Option<String>,
        #[arg(
            long,
            default_value = "debug",
            add = ArgValueCompleter::new(complete_profile)
        )]
        profile: String,
        #[arg(
            long,
            default_value = frostbuild_core::manifest::HOST_PLATFORM,
            add = ArgValueCompleter::new(complete_platform)
        )]
        platform: String,
        #[arg(long)]
        json: bool,
    },
    /// Generate completion code for a shell, or install the dynamic hook
    Completions {
        /// Omit with --install to detect the shell from $SHELL
        #[arg(value_enum)]
        shell: Option<CompletionShell>,
        /// Add the workspace-aware completion hook to this shell's startup file
        #[arg(long)]
        install: bool,
        /// Print what --install would write without touching any file
        #[arg(long, requires = "install")]
        dry_run: bool,
    },
    /// Select build or test targets interactively with fzf
    Pick {
        /// Select only test targets and run `frost test`
        #[arg(long)]
        tests: bool,
        /// Print selected labels instead of building
        #[arg(long)]
        print: bool,
        #[arg(
            long,
            default_value = "debug",
            add = ArgValueCompleter::new(complete_profile)
        )]
        profile: String,
        #[arg(
            long,
            default_value = frostbuild_core::manifest::HOST_PLATFORM,
            add = ArgValueCompleter::new(complete_platform)
        )]
        platform: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum QueryCmd {
    /// Transitive dependencies of a target (itself included)
    Deps {
        #[arg(add = ArgValueCompleter::new(complete_target))]
        target: String,
        #[command(flatten)]
        opts: QueryOpts,
    },
    /// Targets that transitively depend on a target ("what does this affect?")
    Rdeps {
        #[arg(add = ArgValueCompleter::new(complete_target))]
        target: String,
        #[command(flatten)]
        opts: QueryOpts,
    },
    /// One dependency path between two targets
    Somepath {
        #[arg(add = ArgValueCompleter::new(complete_target))]
        from: String,
        #[arg(add = ArgValueCompleter::new(complete_target))]
        to: String,
        #[command(flatten)]
        opts: QueryOpts,
    },
    /// Every dependency path between two targets ("what would I have to cut?")
    Allpaths {
        #[arg(add = ArgValueCompleter::new(complete_target))]
        from: String,
        #[arg(add = ArgValueCompleter::new(complete_target))]
        to: String,
        /// Stop after this many paths. The count is exponential on a graph of
        /// stacked diamonds, so the walk is bounded and says when it stopped.
        #[arg(long, default_value_t = DEFAULT_ALLPATHS_LIMIT)]
        limit: usize,
        #[command(flatten)]
        opts: QueryOpts,
    },
    /// Every target in the workspace
    ///
    /// The one query with no starting point. `deps` and `rdeps` both need a
    /// target to walk from, which makes "what is in this workspace" the
    /// question they cannot answer — tooling was deriving it from the roots of
    /// `--output dot`, which encodes kind in a node *shape* and is a rendering
    /// choice rather than a contract.
    Targets {
        #[command(flatten)]
        opts: QueryOpts,
    },
    /// Targets that declare these files among their action inputs
    Owners {
        /// Workspace-relative paths or globs
        #[arg(required = true, value_hint = ValueHint::FilePath)]
        paths: Vec<String>,
        #[command(flatten)]
        opts: QueryOpts,
    },
}

/// Filters and output formats every query function shares.
#[derive(clap::Args)]
pub(crate) struct QueryOpts {
    /// Keep only targets of this kind (cc_binary, cc_library, cc_test,
    /// genrule, test, kofun_binary, command)
    #[arg(long, value_name = "KIND", add = ArgValueCompleter::new(complete_target_kind))]
    pub(crate) kind: Option<String>,
    /// Keep only targets whose attribute matches, as NAME=PATTERN. Repeatable;
    /// every one must match. NAME is deps, srcs, outputs, sandbox or timeout.
    #[arg(long, value_name = "NAME=PATTERN", add = ArgValueCompleter::new(complete_attr_filter))]
    pub(crate) attr: Vec<String>,
    /// Output format
    #[arg(long, value_enum)]
    pub(crate) output: Option<QueryOutput>,
    /// Alias for --output json, kept for compatibility
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum QueryOutput {
    Text,
    Json,
    LabelKind,
    Dot,
}

/// Enough paths to answer "what would I have to cut" on any graph a person
/// reads, and few enough that a pathological one still returns.
const DEFAULT_ALLPATHS_LIMIT: usize = 4096;

#[derive(Subcommand)]
pub(crate) enum DaemonCmd {
    Start,
    Status,
    Stop,
    Restart,
    #[command(hide = true)]
    Serve,
}

#[derive(Subcommand)]
pub(crate) enum CacheCmd {
    /// Report blob/chunk storage and persistent deduplication ratios
    Stats {
        /// Emit one machine-readable JSON object
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum JournalCmd {
    /// Write this build's action-key material: argv, environment, input
    /// digests, toolchain, profile and platform, in a stable order
    Export {
        /// Where to write it. Defaults to stdout
        #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
        out: Option<PathBuf>,
        #[arg(
            long,
            default_value = frostbuild_core::manifest::DEFAULT_PROFILE,
            add = ArgValueCompleter::new(complete_profile)
        )]
        profile: String,
        #[arg(
            long,
            default_value = frostbuild_core::manifest::HOST_PLATFORM,
            add = ArgValueCompleter::new(complete_platform)
        )]
        platform: String,
    },
    /// Compare two exports and report, per action, the first field that
    /// differs — the cause, not every consequence of it
    Diff {
        #[arg(value_hint = ValueHint::FilePath)]
        first: PathBuf,
        #[arg(value_hint = ValueHint::FilePath)]
        second: PathBuf,
    },
}

/// How much of what the tests wrote reaches the terminal.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum TestOutputArg {
    /// Counts only. For a run whose result is the exit code.
    Summary,
    /// Failing tests, replayed after the run so the log that matters is the
    /// last thing on screen rather than scrolled away by later work.
    Errors,
    /// Everything, passing tests included.
    All,
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum SchedulerArg {
    CriticalPath,
    Fifo,
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum EstimatorArg {
    Heuristic,
    Journal,
    Static,
    Learned,
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    Powershell,
    Elvish,
    Nushell,
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum InitLanguage {
    Native,
    Java,
    Rust,
    Go,
    Typescript,
    Python,
}

impl From<InitLanguage> for frostbuild_core::manifest::ScaffoldLanguage {
    fn from(language: InitLanguage) -> Self {
        use frostbuild_core::manifest::ScaffoldLanguage;
        match language {
            InitLanguage::Native => ScaffoldLanguage::Native,
            InitLanguage::Java => ScaffoldLanguage::Java,
            InitLanguage::Rust => ScaffoldLanguage::Rust,
            InitLanguage::Go => ScaffoldLanguage::Go,
            InitLanguage::Typescript => ScaffoldLanguage::TypeScript,
            InitLanguage::Python => ScaffoldLanguage::Python,
        }
    }
}
