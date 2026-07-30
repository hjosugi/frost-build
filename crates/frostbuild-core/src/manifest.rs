use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::validate_rel_path;

pub const MANIFEST_FILE: &str = "frost.toml";

/// The implicit platform backed by the root `[toolchain]` table. Building for
/// it keeps historical output paths and cache identities unchanged.
pub const HOST_PLATFORM: &str = "host";

/// The profile every workspace has without declaring one.
pub const DEFAULT_PROFILE: &str = "debug";

/// C and C++ driver names a workspace gets without declaring any.
///
/// `cc` and `c++` are the POSIX-conventional names and exist on Linux and
/// macOS. On Windows they generally do not: a MinGW installation provides
/// `gcc`/`g++`, and MSVC provides `cl`. Defaulting to `cc` there meant a
/// scaffolded workspace could not compile anything until the author looked up
/// why `compiler "cc" not found in PATH` appeared on a host we publish a
/// release archive for.
pub fn default_cc() -> &'static str {
    if cfg!(windows) {
        "gcc"
    } else {
        "cc"
    }
}

pub fn default_cxx() -> &'static str {
    if cfg!(windows) {
        "g++"
    } else {
        "c++"
    }
}

/// Archiver flags a workspace gets without declaring any.
///
/// `D` asks for a deterministic archive — identical bytes for identical
/// members, independent of when they were written — which is why it is the
/// default wherever it exists. The cctools `ar` that Xcode ships rejects it
/// outright (`illegal option -- D`), so keeping it unconditional made every
/// archive action fail on a macOS host with a default manifest. There, member
/// identity already comes from the object files Frost tracks, and a workspace
/// that wants the flag can point `ar` at `llvm-ar` and set `arflags`.
pub fn default_arflags() -> &'static [String] {
    static FLAGS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    FLAGS.get_or_init(|| {
        if cfg!(target_os = "macos") {
            vec!["rcs".to_string()]
        } else {
            vec!["rcsD".to_string()]
        }
    })
}

/// The closest candidate to `input`, when one is close enough to be worth
/// suggesting. Turns "unknown X" into "unknown X, did you mean Y".
pub fn closest<'a>(input: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let mut best: Option<(usize, &str)> = None;
    for candidate in candidates {
        let distance = edit_distance(input, candidate);
        if best.is_none() || best.is_some_and(|(d, _)| distance < d) {
            best = Some((distance, candidate));
        }
    }
    // One edit per three characters: short names need a near-exact match,
    // longer ones tolerate a typo or two. A suggestion that is not actually
    // similar is worse than no suggestion.
    let budget = 1 + input.chars().count() / 3;
    best.filter(|&(distance, _)| distance <= budget)
        .map(|(_, name)| name)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        current[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let substitute = previous[j] + usize::from(ca != cb);
            current[j + 1] = substitute.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    CcBinary,
    CcLibrary,
    CcTest,
    Genrule,
    Test,
    KofunBinary,
    Command,
}

impl TargetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TargetKind::CcBinary => "cc_binary",
            TargetKind::CcLibrary => "cc_library",
            TargetKind::CcTest => "cc_test",
            TargetKind::Genrule => "genrule",
            TargetKind::Test => "test",
            TargetKind::KofunBinary => "kofun_binary",
            TargetKind::Command => "command",
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWorkspace {
    #[allow(dead_code)]
    name: Option<String>,
    #[serde(default)]
    default_targets: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawToolchain {
    cc: Option<String>,
    cxx: Option<String>,
    ar: Option<String>,
    kofunc: Option<String>,
    /// Named language/build tools used by `kind = "command"` targets.
    #[serde(default)]
    tools: BTreeMap<String, String>,
    #[serde(default)]
    arflags: Option<Vec<String>>,
    #[serde(default)]
    cflags: Vec<String>,
    #[serde(default)]
    cxxflags: Vec<String>,
    #[serde(default)]
    ldflags: Vec<String>,
}

/// A named build platform: a toolchain overlay for cross/device builds.
/// Unset drivers inherit from the root `[toolchain]`; flags are appended
/// after the root toolchain's flags; `sysroot` expands to `--sysroot=`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPlatform {
    cc: Option<String>,
    cxx: Option<String>,
    ar: Option<String>,
    kofunc: Option<String>,
    /// Per-platform overrides/additions for `[toolchain.tools]`.
    #[serde(default)]
    tools: BTreeMap<String, String>,
    #[serde(default)]
    arflags: Option<Vec<String>>,
    sysroot: Option<String>,
    #[serde(default)]
    cflags: Vec<String>,
    #[serde(default)]
    cxxflags: Vec<String>,
    #[serde(default)]
    ldflags: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    #[serde(default)]
    cflags: Vec<String>,
    #[serde(default)]
    cxxflags: Vec<String>,
    #[serde(default)]
    ldflags: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTarget {
    kind: TargetKind,
    #[serde(default)]
    srcs: Vec<String>,
    #[serde(default)]
    deps: Vec<String>,
    #[serde(default)]
    includes: Vec<String>,
    #[serde(default)]
    cflags: Vec<String>,
    #[serde(default)]
    ldflags: Vec<String>,
    cmd: Option<String>,
    /// Named `[toolchain.tools]` entry used by a command target.
    tool: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    pass_env: Vec<String>,
    /// Additional direct-argv commands run after the primary command.
    #[serde(default)]
    steps: Vec<RawCommandStep>,
    /// Configuration-isolated intermediate directories reset before execution.
    #[serde(default)]
    clean_dirs: Vec<String>,
    /// Keep declared outputs in place while rerunning an incremental compiler.
    #[serde(default)]
    preserve_outputs: bool,
    /// Seconds this action may run before Frost stops it. Absent means the
    /// invocation decides; see `BuildOptions::timeout`.
    timeout: Option<u64>,
    /// Optional dynamic dependency file (Makefile format by default).
    depfile: Option<String>,
    /// Format of the dynamic dependency report; see `depfile::Format`.
    depfile_format: Option<String>,
    #[serde(default)]
    inputs: Vec<String>,
    #[serde(default)]
    outputs: Vec<String>,
    /// Directories whose entire contents are this target's output, for tools
    /// whose output file names cannot be written down in advance.
    #[serde(default)]
    output_dirs: Vec<String>,
    /// Tests may opt out of sandboxing when they intentionally inspect the host.
    sandbox: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCommandStep {
    tool: String,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    #[serde(default)]
    workspace: RawWorkspace,
    #[serde(default)]
    toolchain: RawToolchain,
    #[serde(default)]
    platform: BTreeMap<String, RawPlatform>,
    #[serde(default)]
    profile: BTreeMap<String, RawProfile>,
    #[serde(default)]
    target: BTreeMap<String, RawTarget>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Toolchain {
    pub cc: String,
    pub cxx: String,
    pub ar: String,
    /// Kofun compiler driver. It is optional so C-only workspaces do not need
    /// Kofun installed merely to fingerprint their configured toolchain.
    pub kofunc: Option<String>,
    pub tools: BTreeMap<String, String>,
    pub arflags: Vec<String>,
    pub cflags: Vec<String>,
    pub cxxflags: Vec<String>,
    pub ldflags: Vec<String>,
}

/// Toolchain overlay declared as `[platform.<name>]`; see `RawPlatform`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Platform {
    pub cc: Option<String>,
    pub cxx: Option<String>,
    pub ar: Option<String>,
    pub kofunc: Option<String>,
    pub tools: BTreeMap<String, String>,
    pub arflags: Option<Vec<String>>,
    pub sysroot: Option<String>,
    pub cflags: Vec<String>,
    pub cxxflags: Vec<String>,
    pub ldflags: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Profile {
    pub cflags: Vec<String>,
    pub cxxflags: Vec<String>,
    pub ldflags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Target {
    pub name: String,
    pub kind: TargetKind,
    pub srcs: Vec<String>,
    pub deps: Vec<String>,
    /// Exported include directories, visible to this target and dependents.
    pub includes: Vec<String>,
    pub cflags: Vec<String>,
    pub ldflags: Vec<String>,
    /// Genrule only: shell command with `${in}` / `${out}` / `${outs}`.
    pub cmd: Option<String>,
    /// Command target only: named tool plus direct argv (no shell).
    pub tool: Option<String>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub pass_env: Vec<String>,
    pub steps: Vec<CommandStep>,
    pub clean_dirs: Vec<String>,
    pub preserve_outputs: bool,
    /// Seconds this action may run before it is stopped. A limit is about the
    /// environment, not the result, so it is deliberately not action-key
    /// material (docs/16_action_key_audit.md).
    pub timeout_secs: Option<u64>,
    pub depfile: Option<String>,
    /// How this action reports the inputs it read. `showincludes` is read from
    /// captured output, so it comes without a `depfile` path.
    pub depfile_format: crate::depfile::Format,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    /// Command target only: directories Frost owns entirely. Every file under
    /// one is recorded as an output of the action that declared it.
    pub output_dirs: Vec<String>,
    pub sandbox: bool,
    /// Package directory relative to the workspace root (empty for root).
    pub package: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandStep {
    pub tool: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub default_targets: Vec<String>,
    pub toolchain: Toolchain,
    pub platforms: BTreeMap<String, Platform>,
    pub profiles: BTreeMap<String, Profile>,
    pub targets: BTreeMap<String, Target>,
    /// Manifests which contributed to this workspace, used by graph caching.
    pub manifest_paths: Vec<String>,
}

impl Manifest {
    pub fn load(workspace_root: &Path) -> Result<Self> {
        let path = workspace_root.join(MANIFEST_FILE);
        let text = std::fs::read_to_string(&path).map_err(|_| {
            anyhow::anyhow!(
                "no {MANIFEST_FILE} in {}. run `frost init` to write one, \
                 or `-C <dir>` to build somewhere else",
                workspace_root.display()
            )
        })?;
        let raw: RawManifest =
            toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
        let root_has_workspace = toml::from_str::<toml::Value>(&text)
            .ok()
            .and_then(|v| v.get("workspace").cloned())
            .is_some();
        let mut manifest = Self::from_raw_unvalidated(raw)?;
        for target in manifest.targets.values_mut() {
            target.deps = target
                .deps
                .iter()
                .map(|dep| dep.strip_prefix("//:").unwrap_or(dep).to_string())
                .collect();
        }
        manifest.default_targets = manifest
            .default_targets
            .iter()
            .map(|name| name.strip_prefix("//:").unwrap_or(name).to_string())
            .collect();
        manifest.manifest_paths.push(MANIFEST_FILE.to_string());
        expand_manifest_paths(&mut manifest, workspace_root, "")?;

        if root_has_workspace {
            let mut packages = discover_package_manifests(workspace_root)?;
            packages.sort();
            for rel in packages {
                let package = rel
                    .parent()
                    .unwrap_or_else(|| Path::new(""))
                    .to_str()
                    .context("non-UTF-8 package path is not supported")?
                    .replace('\\', "/");
                if package.is_empty() {
                    continue;
                }
                let package_text = std::fs::read_to_string(workspace_root.join(&rel))
                    .with_context(|| format!("failed to read {}", rel.display()))?;
                let package_raw: RawManifest = toml::from_str(&package_text)
                    .with_context(|| format!("failed to parse {}", rel.display()))?;
                let mut child = Self::from_raw_unvalidated(package_raw)?;
                expand_manifest_paths(&mut child, workspace_root, &package)?;
                for (local, mut target) in child.targets {
                    let canonical = format!("//{package}:{local}");
                    target.name = canonical.clone();
                    target.package = package.clone();
                    target.deps = target
                        .deps
                        .iter()
                        .map(|dep| resolve_label(dep, &package))
                        .collect();
                    if manifest.targets.insert(canonical.clone(), target).is_some() {
                        bail!("duplicate target label {canonical:?}");
                    }
                }
                manifest.manifest_paths.push(
                    rel.to_str()
                        .context("non-UTF-8 manifest path is not supported")?
                        .replace('\\', "/"),
                );
            }
        }
        validate_dependencies(&manifest.targets)?;
        validate_default_targets(&manifest)?;
        Ok(manifest)
    }

    pub fn parse_str(text: &str) -> Result<Self> {
        let raw: RawManifest = toml::from_str(text).context("failed to parse manifest")?;
        Self::from_raw(raw)
    }

    /// Resolves the effective toolchain for a platform: the root `[toolchain]`
    /// for `host`, otherwise that toolchain with the `[platform.<name>]`
    /// overlay applied (driver overrides, appended flags, sysroot expansion).
    pub fn toolchain_for(&self, platform: &str) -> Result<Toolchain> {
        if platform == HOST_PLATFORM {
            return Ok(self.toolchain.clone());
        }
        let Some(spec) = self.platforms.get(platform) else {
            let known: Vec<&str> = self.platforms.keys().map(String::as_str).collect();
            if let Some(hint) = closest(platform, known.iter().copied()) {
                bail!("unknown platform {platform:?}. did you mean {hint:?}?");
            }
            bail!(
                "unknown platform {platform:?}{}",
                if known.is_empty() {
                    ". this workspace declares no [platform.*] sections".to_string()
                } else {
                    format!(". declared platforms: {}", known.join(", "))
                }
            );
        };
        let base = &self.toolchain;
        let mut resolved = Toolchain {
            cc: spec.cc.clone().unwrap_or_else(|| base.cc.clone()),
            cxx: spec.cxx.clone().unwrap_or_else(|| base.cxx.clone()),
            ar: spec.ar.clone().unwrap_or_else(|| base.ar.clone()),
            kofunc: spec.kofunc.clone().or_else(|| base.kofunc.clone()),
            tools: base.tools.clone(),
            arflags: spec.arflags.clone().unwrap_or_else(|| base.arflags.clone()),
            cflags: base.cflags.clone(),
            cxxflags: base.cxxflags.clone(),
            ldflags: base.ldflags.clone(),
        };
        resolved.tools.extend(spec.tools.clone());
        if let Some(sysroot) = &spec.sysroot {
            let flag = format!("--sysroot={sysroot}");
            resolved.cflags.push(flag.clone());
            resolved.ldflags.push(flag);
        }
        resolved.cflags.extend(spec.cflags.iter().cloned());
        resolved.cxxflags.extend(spec.cxxflags.iter().cloned());
        resolved.ldflags.extend(spec.ldflags.iter().cloned());
        Ok(resolved)
    }

    fn from_raw(raw: RawManifest) -> Result<Self> {
        let manifest = Self::from_raw_unvalidated(raw)?;
        if manifest.targets.is_empty() {
            bail!("manifest declares no [target.*] sections");
        }
        validate_dependencies(&manifest.targets)?;
        validate_default_targets(&manifest)?;
        Ok(manifest)
    }

    fn from_raw_unvalidated(raw: RawManifest) -> Result<Self> {
        let mut targets = BTreeMap::new();
        for (name, spec) in raw.target {
            let target =
                build_target(&name, spec).with_context(|| format!("invalid target {name:?}"))?;
            targets.insert(name, target);
        }

        let default_targets = if raw.workspace.default_targets.is_empty() {
            let binaries: Vec<String> = targets
                .values()
                .filter(|t| matches!(t.kind, TargetKind::CcBinary | TargetKind::KofunBinary))
                .map(|t| t.name.clone())
                .collect();
            if binaries.is_empty() {
                targets.keys().cloned().collect()
            } else {
                binaries
            }
        } else {
            raw.workspace.default_targets
        };

        let mut platforms = BTreeMap::new();
        for (name, spec) in raw.platform {
            if name == HOST_PLATFORM {
                bail!("platform name {HOST_PLATFORM:?} is reserved for the root [toolchain]");
            }
            if !valid_target_name(&name) {
                bail!("platform name must match [A-Za-z0-9_-]+, got {name:?}");
            }
            platforms.insert(
                name,
                Platform {
                    cc: spec.cc,
                    cxx: spec.cxx,
                    ar: spec.ar,
                    kofunc: spec.kofunc,
                    tools: spec.tools,
                    arflags: spec.arflags,
                    sysroot: spec.sysroot,
                    cflags: spec.cflags,
                    cxxflags: spec.cxxflags,
                    ldflags: spec.ldflags,
                },
            );
        }

        Ok(Self {
            default_targets,
            toolchain: Toolchain {
                cc: raw.toolchain.cc.unwrap_or_else(|| default_cc().to_string()),
                cxx: raw
                    .toolchain
                    .cxx
                    .unwrap_or_else(|| default_cxx().to_string()),
                ar: raw.toolchain.ar.unwrap_or_else(|| "ar".to_string()),
                kofunc: raw.toolchain.kofunc,
                tools: raw.toolchain.tools,
                arflags: raw
                    .toolchain
                    .arflags
                    .unwrap_or_else(|| default_arflags().to_vec()),
                cflags: raw.toolchain.cflags,
                cxxflags: raw.toolchain.cxxflags,
                ldflags: raw.toolchain.ldflags,
            },
            platforms,
            profiles: raw
                .profile
                .into_iter()
                .map(|(name, p)| {
                    (
                        name,
                        Profile {
                            cflags: p.cflags,
                            cxxflags: p.cxxflags,
                            ldflags: p.ldflags,
                        },
                    )
                })
                .collect(),
            targets,
            manifest_paths: Vec::new(),
        })
    }
}

fn valid_target_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn build_target(name: &str, spec: RawTarget) -> Result<Target> {
    if !valid_target_name(name) {
        bail!("target name must match [A-Za-z0-9_-]+");
    }

    let srcs = validate_paths(&spec.srcs).context("srcs")?;
    let includes = validate_paths(&spec.includes).context("includes")?;
    let inputs = validate_paths(&spec.inputs).context("inputs")?;
    let outputs = validate_paths(&spec.outputs).context("outputs")?;
    let output_dirs = validate_paths(&spec.output_dirs).context("output_dirs")?;
    let clean_dirs = validate_paths(&spec.clean_dirs).context("clean_dirs")?;
    let depfile = spec
        .depfile
        .as_deref()
        .map(validate_rel_path)
        .transpose()
        .context("depfile")?;
    let depfile_format = match spec.depfile_format.as_deref() {
        None | Some("make") => crate::depfile::Format::Make,
        Some("lines") => crate::depfile::Format::Lines,
        Some("showincludes") => crate::depfile::Format::ShowIncludes,
        Some(other) => {
            bail!("unknown depfile_format {other:?} (supported: make, lines, showincludes)")
        }
    };
    // `showincludes` is read from captured output, because that is where MSVC
    // writes it; every other format names a file.
    if depfile_format.reads_captured_output() {
        if depfile.is_some() {
            bail!("depfile_format = \"showincludes\" reads captured output and takes no depfile");
        }
    } else if spec.depfile_format.is_some() && depfile.is_none() {
        bail!(
            "depfile_format = {:?} requires a depfile path",
            depfile_format.as_str()
        );
    }
    let has_command_fields = spec.tool.is_some()
        || !spec.args.is_empty()
        || !spec.env.is_empty()
        || !spec.pass_env.is_empty()
        || !spec.steps.is_empty()
        || !clean_dirs.is_empty()
        || spec.preserve_outputs
        || depfile.is_some()
        || spec.depfile_format.is_some()
        || !output_dirs.is_empty();

    match spec.kind {
        TargetKind::CcBinary | TargetKind::CcLibrary | TargetKind::CcTest => {
            if srcs.is_empty() {
                bail!("{} requires non-empty srcs", spec.kind.as_str());
            }
            if spec.cmd.is_some() || !inputs.is_empty() || !outputs.is_empty() {
                bail!(
                    "{} must not set genrule fields (cmd/inputs/outputs)",
                    spec.kind.as_str()
                );
            }
            if has_command_fields {
                bail!("{} must not set command adapter fields", spec.kind.as_str());
            }
        }
        TargetKind::KofunBinary => {
            validate_kofun_binary_sources(&srcs)?;
            if !includes.is_empty() || !spec.cflags.is_empty() || !spec.ldflags.is_empty() {
                bail!("kofun_binary does not support includes/cflags/ldflags");
            }
            if spec.cmd.is_some() || !inputs.is_empty() || !outputs.is_empty() {
                bail!("kofun_binary must not set genrule fields (cmd/inputs/outputs)");
            }
            if has_command_fields {
                bail!("kofun_binary must not set command adapter fields");
            }
        }
        TargetKind::Genrule => {
            if spec.cmd.as_deref().map(str::trim).unwrap_or("").is_empty() {
                bail!("genrule requires a non-empty cmd");
            }
            if outputs.is_empty() {
                bail!("genrule requires non-empty outputs");
            }
            if !srcs.is_empty() {
                bail!("genrule uses inputs, not srcs");
            }
            if !spec.cflags.is_empty() || !spec.ldflags.is_empty() {
                bail!("genrule must not set cflags/ldflags");
            }
            if has_command_fields {
                bail!("genrule must not set command adapter fields");
            }
        }
        TargetKind::Test => {
            let has_shell = spec
                .cmd
                .as_deref()
                .is_some_and(|cmd| !cmd.trim().is_empty());
            let has_direct = spec.tool.is_some();
            if has_shell == has_direct {
                bail!("test requires exactly one of cmd or tool");
            }
            if !outputs.is_empty() {
                bail!("test success output is managed by Frost; outputs must be empty");
            }
            if !srcs.is_empty() {
                bail!("test uses inputs, not srcs");
            }
            if !spec.cflags.is_empty() || !spec.ldflags.is_empty() {
                bail!("test must not set cflags/ldflags");
            }
            if has_shell && has_command_fields {
                bail!("shell test must not set command adapter fields");
            }
            if has_direct {
                let tool = spec.tool.as_deref().map(str::trim).unwrap_or("");
                if !valid_target_name(tool) {
                    bail!("direct test requires tool = \"NAME\" matching [A-Za-z0-9_-]+");
                }
                if !spec.steps.is_empty()
                    || !clean_dirs.is_empty()
                    || spec.preserve_outputs
                    || depfile.is_some()
                {
                    bail!(
                        "direct test does not support steps, clean_dirs, preserve_outputs or depfile"
                    );
                }
                for name in spec.env.keys().chain(&spec.pass_env) {
                    if !valid_env_name(name) {
                        bail!("invalid environment variable name {name:?}");
                    }
                }
                let mut pass_env = spec.pass_env.clone();
                pass_env.sort();
                if pass_env.windows(2).any(|pair| pair[0] == pair[1]) {
                    bail!("test pass_env contains a duplicate name");
                }
                if pass_env.iter().any(|name| spec.env.contains_key(name)) {
                    bail!("test env and pass_env must not contain the same name");
                }
            }
        }
        TargetKind::Command => {
            let tool = spec.tool.as_deref().map(str::trim).unwrap_or("");
            if !valid_target_name(tool) {
                bail!("command requires tool = \"NAME\" matching [A-Za-z0-9_-]+");
            }
            if outputs.is_empty() && output_dirs.is_empty() {
                bail!("command requires non-empty outputs or output_dirs");
            }
            if outputs.iter().any(|output| !output.contains("${config}")) {
                bail!(
                    "command outputs must contain ${{config}} so profile/platform builds stay isolated"
                );
            }
            if output_dirs.iter().any(|dir| !dir.contains("${config}")) {
                bail!(
                    "command output_dirs must contain ${{config}} so profile/platform builds stay isolated"
                );
            }
            // Frost deletes and republishes an owned directory wholesale, so
            // anything else that names a path inside one would be describing
            // the same bytes under two different ownership rules.
            for dir in &output_dirs {
                let prefix = format!("{}/", dir.trim_end_matches('/'));
                if let Some(other) = output_dirs
                    .iter()
                    .find(|other| *other != dir && other.starts_with(&prefix))
                {
                    bail!("command output_dir {other:?} is nested inside {dir:?}");
                }
                if let Some(output) = outputs.iter().find(|output| output.starts_with(&prefix)) {
                    bail!("command output {output:?} is inside output_dir {dir:?}");
                }
                if let Some(clean) = clean_dirs
                    .iter()
                    .find(|clean| clean.starts_with(&prefix) || **clean == *dir)
                {
                    bail!("command clean_dir {clean:?} is inside output_dir {dir:?}");
                }
                if depfile
                    .as_ref()
                    .is_some_and(|path| path.starts_with(&prefix))
                {
                    bail!("command depfile must not be inside output_dir {dir:?}");
                }
            }
            if depfile
                .as_ref()
                .is_some_and(|path| !path.contains("${config}"))
            {
                bail!("command depfile must contain ${{config}}");
            }
            if clean_dirs.iter().any(|path| !path.contains("${config}")) {
                bail!("command clean_dirs must contain ${{config}}");
            }
            if spec.cmd.is_some()
                || !srcs.is_empty()
                || !includes.is_empty()
                || !spec.cflags.is_empty()
                || !spec.ldflags.is_empty()
            {
                bail!("command uses tool/args/inputs/outputs, not C or genrule fields");
            }
            for name in spec.env.keys().chain(&spec.pass_env) {
                if !valid_env_name(name) {
                    bail!("invalid environment variable name {name:?}");
                }
            }
            let mut pass_env = spec.pass_env.clone();
            pass_env.sort();
            if pass_env.windows(2).any(|pair| pair[0] == pair[1]) {
                bail!("command pass_env contains a duplicate name");
            }
            if pass_env.iter().any(|name| spec.env.contains_key(name)) {
                bail!("command env and pass_env must not contain the same name");
            }
            for step in &spec.steps {
                if !valid_target_name(&step.tool) {
                    bail!(
                        "command step tool {:?} must match [A-Za-z0-9_-]+",
                        step.tool
                    );
                }
            }
        }
    }

    Ok(Target {
        name: name.to_string(),
        kind: spec.kind,
        srcs,
        deps: spec.deps,
        includes,
        cflags: spec.cflags,
        ldflags: spec.ldflags,
        cmd: spec.cmd,
        tool: spec.tool,
        args: spec.args,
        env: spec.env,
        pass_env: spec.pass_env,
        steps: spec
            .steps
            .into_iter()
            .map(|step| CommandStep {
                tool: step.tool,
                args: step.args,
            })
            .collect(),
        clean_dirs,
        preserve_outputs: spec.preserve_outputs,
        timeout_secs: spec.timeout,
        depfile,
        depfile_format,
        inputs,
        outputs,
        output_dirs,
        sandbox: spec.sandbox.unwrap_or(true),
        package: String::new(),
    })
}

fn discover_package_manifests(root: &Path) -> Result<Vec<PathBuf>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            if matches!(name.to_str(), Some(".git" | ".frost" | "target")) {
                continue;
            }
            let ty = entry.file_type()?;
            if ty.is_dir() && !ty.is_symlink() {
                walk(root, &entry.path(), out)?;
            } else if ty.is_file() && name == MANIFEST_FILE {
                out.push(entry.path().strip_prefix(root).unwrap().to_path_buf());
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    Ok(out)
}

fn resolve_label(raw: &str, package: &str) -> String {
    if let Some(root) = raw.strip_prefix("//:") {
        root.to_string()
    } else if raw.starts_with("//") {
        raw.to_string()
    } else {
        format!("//{package}:{}", raw.trim_start_matches(':'))
    }
}

fn prefix_path(package: &str, path: &str) -> String {
    if package.is_empty() {
        path.to_string()
    } else {
        format!("{package}/{path}")
    }
}

fn has_glob(path: &str) -> bool {
    path.bytes().any(|b| matches!(b, b'*' | b'?' | b'['))
}

fn expand_paths(root: &Path, package: &str, paths: &[String]) -> Result<Vec<String>> {
    let mut expanded = Vec::new();
    let mut ignore_builder = ignore::gitignore::GitignoreBuilder::new(root);
    for file in [".gitignore", ".frostignore"] {
        let path = root.join(file);
        if path.exists() {
            ignore_builder.add(path);
        }
    }
    let ignored = ignore_builder.build()?;
    for path in paths {
        let rel = prefix_path(package, path);
        if !has_glob(path) {
            expanded.push(rel);
            continue;
        }
        let pattern = root.join(&rel).to_string_lossy().to_string();
        let matches = glob::glob(&pattern).with_context(|| format!("invalid glob {path:?}"))?;
        let before = expanded.len();
        for item in matches {
            let item = item.with_context(|| format!("failed to expand glob {path:?}"))?;
            if !item.is_file() {
                continue;
            }
            let relative = item
                .strip_prefix(root)
                .context("glob escaped workspace")?
                .to_str()
                .context("non-UTF-8 source path is not supported")?
                .replace('\\', "/");
            if !relative.starts_with(".frost/")
                && !relative.starts_with(".git/")
                && !ignored
                    .matched_path_or_any_parents(&item, false)
                    .is_ignore()
            {
                expanded.push(relative);
            }
        }
        // A pattern that matches nothing is a typo far more often than an
        // intent, and the damage shows up somewhere else: a cc_library whose
        // srcs vanished still archives, and the build fails at the link with
        // a message about symbols rather than about the glob. Say it here,
        // where the cause is.
        if expanded.len() == before {
            bail!("{path:?} matched no files");
        }
    }
    expanded.sort();
    expanded.dedup();
    Ok(expanded)
}

fn expand_manifest_paths(manifest: &mut Manifest, root: &Path, package: &str) -> Result<()> {
    for (name, target) in manifest.targets.iter_mut() {
        target.package = package.to_string();
        target.srcs = expand_paths(root, package, &target.srcs)
            .with_context(|| format!("target {name:?} srcs"))?;
        if target.kind == TargetKind::KofunBinary {
            validate_kofun_binary_sources(&target.srcs)
                .with_context(|| format!("target {name:?} expanded srcs"))?;
        }
        target.inputs = expand_paths(root, package, &target.inputs)
            .with_context(|| format!("target {name:?} inputs"))?;
        target.includes = target
            .includes
            .iter()
            .map(|p| prefix_path(package, p))
            .collect();
        target.outputs = target
            .outputs
            .iter()
            .map(|p| prefix_path(package, p))
            .collect();
        target.output_dirs = target
            .output_dirs
            .iter()
            .map(|p| prefix_path(package, p))
            .collect();
        target.clean_dirs = target
            .clean_dirs
            .iter()
            .map(|p| prefix_path(package, p))
            .collect();
        target.depfile = target
            .depfile
            .as_ref()
            .map(|path| prefix_path(package, path));
    }
    Ok(())
}

fn validate_kofun_binary_sources(srcs: &[String]) -> Result<()> {
    if srcs.len() != 1 {
        bail!("kofun_binary requires exactly one source");
    }
    if !srcs[0].ends_with(".kofun") {
        bail!("kofun_binary source must use the .kofun extension");
    }
    Ok(())
}

fn validate_dependencies(targets: &BTreeMap<String, Target>) -> Result<()> {
    for target in targets.values() {
        for dep in &target.deps {
            if dep == &target.name {
                bail!("target {:?} depends on itself", target.name);
            }
            if !targets.contains_key(dep) {
                bail!("target {:?} has unknown dep {dep:?}", target.name);
            }
        }
    }
    Ok(())
}

fn validate_default_targets(manifest: &Manifest) -> Result<()> {
    for name in &manifest.default_targets {
        if !manifest.targets.contains_key(name) {
            bail!("workspace.default_targets names unknown target {name:?}");
        }
    }
    Ok(())
}

fn validate_paths(raw: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(raw.len());
    for p in raw {
        out.push(validate_rel_path(p)?);
    }
    Ok(out)
}

/// Language families whose build artifact can be scaffolded without guessing
/// package-manager or dynamic-output semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaffoldLanguage {
    Native,
    Java,
    Rust,
    Go,
    TypeScript,
    Python,
}

impl ScaffoldLanguage {
    /// The `--language` spelling, also used in every diagnostic so the message
    /// names a flag value the caller can paste back.
    pub fn as_str(self) -> &'static str {
        match self {
            ScaffoldLanguage::Native => "native",
            ScaffoldLanguage::Java => "java",
            ScaffoldLanguage::Rust => "rust",
            ScaffoldLanguage::Go => "go",
            ScaffoldLanguage::TypeScript => "typescript",
            ScaffoldLanguage::Python => "python",
        }
    }
}

/// A starter manifest for a directory that has supported sources but no
/// `frost.toml` yet.
///
/// Deliberately shallow: it reports what it found and writes the smallest
/// manifest that builds it, rather than inferring a target layout the author
/// did not ask for. Anything beyond one binary and one library is a decision
/// the author should make in the file, where it is visible.
#[derive(Debug)]
pub struct Scaffold {
    pub manifest: String,
    /// What the scan saw, for the caller to print.
    pub summary: Vec<String>,
}

const SOURCE_EXTENSIONS: [&str; 6] = ["c", "cc", "cpp", "cxx", "C", "c++"];
const JAVA_EXTENSIONS: [&str; 1] = ["java"];
const RUST_EXTENSIONS: [&str; 1] = ["rs"];
const GO_EXTENSIONS: [&str; 1] = ["go"];
const TYPESCRIPT_EXTENSIONS: [&str; 2] = ["ts", "tsx"];
const PYTHON_EXTENSIONS: [&str; 1] = ["py"];
const JAVA_PROJECT_MARKERS: [&str; 7] = [
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "gradlew",
    "mvnw",
];

const FAMILIES: [(ScaffoldLanguage, &[&str]); 6] = [
    (ScaffoldLanguage::Native, &SOURCE_EXTENSIONS),
    (ScaffoldLanguage::Java, &JAVA_EXTENSIONS),
    (ScaffoldLanguage::Rust, &RUST_EXTENSIONS),
    (ScaffoldLanguage::Go, &GO_EXTENSIONS),
    (ScaffoldLanguage::TypeScript, &TYPESCRIPT_EXTENSIONS),
    (ScaffoldLanguage::Python, &PYTHON_EXTENSIONS),
];

pub fn scaffold(root: &Path) -> Result<Scaffold> {
    let mut found = Vec::new();
    for (language, extensions) in FAMILIES {
        let mut sources = Vec::new();
        collect_sources(root, root, &mut sources, 0, extensions)?;
        if !sources.is_empty() {
            found.push((language, sources.len()));
        }
    }
    match found.as_slice() {
        [(language, _)] => {
            // Auto-detection refuses what a package manager already owns.
            // `--language` stays the deliberate override.
            reject_owned_by_package_manager(root, *language)?;
            scaffold_for(root, *language)
        }
        [] => bail!(
            "no safely scaffoldable C/C++, Java, Rust, Go, TypeScript or Python sources \
             under {}. use a kind = \"command\" target with [toolchain.tools] for Gradle, \
             Maven, npm or another tool that owns its own build",
            root.display()
        ),
        many => bail!(
            "sources for several languages were found under {} ({}). choose one of {} so \
             init does not silently omit half of a polyglot workspace",
            root.display(),
            many.iter()
                .map(|(language, count)| format!("{} x{count}", language.as_str()))
                .collect::<Vec<_>>()
                .join(", "),
            many.iter()
                .map(|(language, _)| format!("`frost init --language {}`", language.as_str()))
                .collect::<Vec<_>>()
                .join(", "),
        ),
    }
}

pub fn scaffold_for(root: &Path, language: ScaffoldLanguage) -> Result<Scaffold> {
    match language {
        ScaffoldLanguage::Native => scaffold_native(root),
        ScaffoldLanguage::Java => scaffold_java(root),
        ScaffoldLanguage::Rust => scaffold_rust(root),
        ScaffoldLanguage::Go => scaffold_go(root),
        ScaffoldLanguage::TypeScript => scaffold_typescript(root),
        ScaffoldLanguage::Python => scaffold_python(root),
    }
}

/// Auto-detection stops where another tool owns dependency resolution, plugin
/// or task semantics. Guessing there produces a manifest that looks right and
/// builds something else, which is worse than refusing.
fn reject_owned_by_package_manager(root: &Path, language: ScaffoldLanguage) -> Result<()> {
    match language {
        ScaffoldLanguage::Native => {
            let bazel = sorted_named_files(
                root,
                &[
                    "MODULE.bazel",
                    "WORKSPACE",
                    "WORKSPACE.bazel",
                    "BUILD",
                    "BUILD.bazel",
                ],
            )?;
            if !bazel.is_empty() {
                bail!(
                    "native sources and an existing Bazel project marker ({}) were found. \
                     init will not bypass Bazel's configured graph: run \
                     `frost import-bazel --dry-run` to review an import, or use \
                     `frost init --language native` only if a direct C/C++ build is intentional",
                    bazel.join(", ")
                );
            }
            let ninja = sorted_named_files(root, &["build.ninja"])?;
            if !ninja.is_empty() {
                bail!(
                    "native sources and an existing Ninja graph ({}) were found. init will not \
                     bypass its generated edges: run `frost import-ninja build.ninja` to import \
                     the supported subset, or use `frost init --language native` only if a \
                     direct C/C++ build is intentional",
                    ninja.join(", ")
                );
            }
            Ok(())
        }
        ScaffoldLanguage::Java => {
            let markers = sorted_named_files(root, &JAVA_PROJECT_MARKERS)?;
            if markers.is_empty() {
                return Ok(());
            }
            bail!(
                "Java sources and an existing Gradle/Maven project marker ({}) were found. \
                 init will not bypass dependency, plugin or task semantics: declare that \
                 build as a kind = \"command\" boundary, or use `frost init --language \
                 java` only if a direct javac/JAR build is intentional",
                markers.join(", ")
            )
        }
        ScaffoldLanguage::Rust => {
            let owning: Vec<String> = sorted_named_files(root, &["Cargo.toml"])?
                .into_iter()
                .filter(|manifest| cargo_manifest_owns_the_build(&root.join(manifest)))
                .collect();
            let build_scripts = sorted_named_files(root, &["build.rs"])?;
            if owning.is_empty() && build_scripts.is_empty() {
                return Ok(());
            }
            bail!(
                "Cargo already owns this build ({}). init will not clone Cargo's dependency, \
                 feature or build-script semantics: keep `cargo` behind a kind = \"command\" \
                 target, or use `frost init --language rust` only if a direct rustc build of \
                 this crate is intentional (docs/19_rust_cargo_comparison.md)",
                owning
                    .into_iter()
                    .chain(build_scripts)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        ScaffoldLanguage::Go => {
            let owning: Vec<String> = sorted_named_files(root, &["go.mod"])?
                .into_iter()
                .filter(|module| go_module_requires_dependencies(&root.join(module)))
                .collect();
            if owning.is_empty() {
                return Ok(());
            }
            bail!(
                "go.mod declares module requirements ({}). init will not resolve the module \
                 graph: keep `go build` behind a kind = \"command\" target, or use \
                 `frost init --language go` only if a direct build of this package is intentional",
                owning.join(", ")
            )
        }
        ScaffoldLanguage::TypeScript => {
            let owning: Vec<String> = sorted_named_files(root, &["package.json"])?
                .into_iter()
                .filter(|package| package_json_declares_dependencies(&root.join(package)))
                .collect();
            if owning.is_empty() {
                return Ok(());
            }
            bail!(
                "package.json declares dependencies ({}). npm owns that graph and its scripts: \
                 run `frost import-npm` to import the validation gates and explicit build \
                 boundaries, or use `frost init --language typescript` only if a direct tsc \
                 build is intentional",
                owning.join(", ")
            )
        }
        ScaffoldLanguage::Python => {
            let owning: Vec<String> = sorted_named_files(root, &["pyproject.toml"])?
                .into_iter()
                .filter(|project| pyproject_declares_dependencies(&root.join(project)))
                .collect();
            if owning.is_empty() {
                return Ok(());
            }
            bail!(
                "pyproject.toml declares runtime dependencies ({}). init packs a pure-Python \
                 tree and does not resolve or vendor an environment: keep the installer behind \
                 a kind = \"command\" target, or use `frost init --language python` only if \
                 packing this source tree is intentional",
                owning.join(", ")
            )
        }
    }
}

fn sorted_named_files(root: &Path, names: &[&str]) -> Result<Vec<String>> {
    let mut found = Vec::new();
    collect_named_files(root, root, &mut found, 0, names)?;
    found.sort();
    Ok(found)
}

/// Textual, like [`defines_main`]: a scaffold may be wrong here because the
/// author reads what it wrote, and the failure mode is a refusal, not a
/// silently different build.
fn cargo_manifest_owns_the_build(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    text.lines().map(str::trim).any(|line| {
        matches!(
            line,
            "[dependencies]" | "[build-dependencies]" | "[workspace]" | "[patch.crates-io]"
        ) || line.starts_with("[dependencies.")
            || line.starts_with("[target.")
            || line.starts_with("[workspace.")
    })
}

fn go_module_requires_dependencies(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    text.lines()
        .map(str::trim)
        .any(|line| line == "require (" || line.starts_with("require "))
}

fn package_json_declares_dependencies(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    ["\"dependencies\"", "\"devDependencies\"", "\"scripts\""]
        .iter()
        .any(|key| match text.split_once(key) {
            // An empty object is a declaration of nothing.
            Some((_, rest)) => !rest.trim_start().starts_with(": {}"),
            None => false,
        })
}

fn pyproject_declares_dependencies(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    text.lines().map(str::trim).any(|line| {
        line == "[tool.poetry.dependencies]"
            || line == "[project.optional-dependencies]"
            || (line.starts_with("dependencies") && !line.replace(' ', "").ends_with("=[]"))
    })
}

fn scaffold_native(root: &Path) -> Result<Scaffold> {
    let mut sources: Vec<String> = Vec::new();
    collect_sources(root, root, &mut sources, 0, &SOURCE_EXTENSIONS)?;
    sources.sort();
    if sources.is_empty() {
        bail!(
            "no C or C++ sources under {}; choose another --language or add a \
             direct command target",
            root.display()
        );
    }

    let has_include = root.join("include").is_dir();
    // A file defining main is the binary; everything else is library code.
    let entry = sources
        .iter()
        .find(|path| defines_main(&root.join(path)))
        .cloned();

    let mut summary = vec![format!("{} source file(s)", sources.len())];
    let mut manifest = String::from("[workspace]\n");

    let (binary_srcs, library_srcs): (Vec<String>, Vec<String>) = match &entry {
        Some(entry) => {
            summary.push(format!("entry point: {entry}"));
            (
                vec![entry.clone()],
                sources.iter().filter(|s| *s != entry).cloned().collect(),
            )
        }
        None => {
            summary.push("no main() found, so everything becomes a library".into());
            (Vec::new(), sources.clone())
        }
    };
    if has_include {
        summary.push("include/ used as the exported header directory".into());
    }

    let name = root
        .file_name()
        .and_then(|n| n.to_str())
        .map(sanitize_name)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "app".to_string());
    let lib_name = format!("{name}_lib");

    let default_target = if binary_srcs.is_empty() {
        // Nothing defines main, so a library is the only honest default.
        name.clone()
    } else {
        name.clone()
    };
    manifest.push_str(&format!("default_targets = [\"{default_target}\"]\n\n"));
    // The drivers are written out rather than left implicit so the manifest
    // stays reviewable, and they are the host's conventional names: `cc`/`c++`
    // on Unix, `gcc`/`g++` where those do not exist.
    manifest.push_str(&format!(
        "[toolchain]\ncc = \"{}\"\ncxx = \"{}\"\ncflags = [\"-Wall\"]\n\n\
         [profile.debug]\ncflags = [\"-O0\", \"-g\"]\n\n\
         [profile.release]\ncflags = [\"-O3\", \"-DNDEBUG\"]\n\n",
        default_cc(),
        default_cxx(),
    ));

    if binary_srcs.is_empty() {
        manifest.push_str(&format!("[target.{name}]\nkind = \"cc_library\"\n"));
        manifest.push_str(&format!("srcs = {}\n", toml_array(&library_srcs)));
        if has_include {
            manifest.push_str("includes = [\"include\"]\n");
        }
    } else {
        if !library_srcs.is_empty() {
            manifest.push_str(&format!("[target.{lib_name}]\nkind = \"cc_library\"\n"));
            manifest.push_str(&format!("srcs = {}\n", toml_array(&library_srcs)));
            if has_include {
                manifest.push_str("includes = [\"include\"]\n");
            }
            manifest.push('\n');
        }
        manifest.push_str(&format!("[target.{name}]\nkind = \"cc_binary\"\n"));
        manifest.push_str(&format!("srcs = {}\n", toml_array(&binary_srcs)));
        if !library_srcs.is_empty() {
            manifest.push_str(&format!("deps = [\"{lib_name}\"]\n"));
        } else if has_include {
            manifest.push_str("includes = [\"include\"]\n");
        }
    }

    Ok(Scaffold { manifest, summary })
}

fn scaffold_java(root: &Path) -> Result<Scaffold> {
    let mut sources = Vec::new();
    collect_sources(root, root, &mut sources, 0, &JAVA_EXTENSIONS)?;
    sources.sort();
    if sources.is_empty() {
        bail!(
            "no Java sources under {}; choose another --language or add a \
             direct command target",
            root.display()
        );
    }
    let name = root
        .file_name()
        .and_then(|value| value.to_str())
        .map(sanitize_name)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "app".to_string());
    let mut main_classes = sources
        .iter()
        .filter_map(|source| java_main_class(&root.join(source)))
        .collect::<Vec<_>>();
    main_classes.sort();
    main_classes.dedup();

    let mut summary = vec![format!("{} Java source file(s)", sources.len())];
    match main_classes.as_slice() {
        [] => summary.push("no public static void main found; generating a library JAR".into()),
        [main] => summary.push(format!("entry point: {main}")),
        [main, rest @ ..] => summary.push(format!(
            "entry point: {main} (selected first of {}; review generated main class)",
            rest.len() + 1
        )),
    }

    let classes = format!(".frost/tmp/${{config}}/java/{name}");
    let output = format!(".frost/out/${{config}}/{name}.jar");
    let mut pack_args = vec![
        "pack-jar".to_string(),
        "--input".to_string(),
        "${clean_dir}".to_string(),
        "--output".to_string(),
        "${out}".to_string(),
    ];
    if let Some(main) = main_classes.first() {
        pack_args.extend(["--main-class".to_string(), main.clone()]);
    }
    let outputs = toml_array(std::slice::from_ref(&output));
    let manifest = format!(
        "[workspace]\ndefault_targets = [\"{name}\"]\n\n\
         [toolchain.tools]\njavac = \"javac\"\nfrost = \"frost\"\n\n\
         [target.{name}]\nkind = \"command\"\ntool = \"javac\"\n\
         args = [\"-encoding\", \"UTF-8\", \"-g\", \"-d\", \"${{clean_dir}}\", \"${{in}}\"]\n\
         inputs = {}\noutputs = {outputs}\nclean_dirs = [{classes:?}]\n\
         steps = [{{ tool = \"frost\", args = {} }}]\n\
         # `javac` and `java` are stubs on macOS that pick the JDK from\n\
         # JAVA_HOME, so a build that cleared it would compile for a different\n\
         # JDK than the one the developer runs. Its value is action-key\n\
         # material, so switching JDKs invalidates rather than mixes.\n\
         pass_env = [\"JAVA_HOME\"]\nsandbox = false\n",
        toml_array(&sources),
        toml_array(&pack_args),
    );
    Ok(Scaffold { manifest, summary })
}

fn scaffold_rust(root: &Path) -> Result<Scaffold> {
    let sources = sorted_sources(root, &RUST_EXTENSIONS, "Rust")?;
    let Some(entry) = sources
        .iter()
        .find(|path| contains_declaration(&root.join(path), "fn main("))
        .cloned()
    else {
        bail!(
            "no `fn main` under {}: a library-only crate has no single artifact init can \
             name. keep cargo as the packaging owner behind a kind = \"command\" target",
            root.display()
        )
    };

    let name = workspace_name(root);
    let output = format!(".frost/out/${{config}}/{name}");
    let summary = vec![
        format!("{} Rust source file(s)", sources.len()),
        format!("entry point: {entry}"),
        "rustc is driven directly; cargo remains the owner of dependencies and packaging".into(),
    ];
    let manifest = format!(
        "# Generated by `frost init` after detecting Rust sources and one crate root.\n\
         # Next: frost build\n\
         # TODO: review the inferred Rust edition and direct-rustc boundary.\n\
         [workspace]\ndefault_targets = [\"{name}\"]\n\n\
         [toolchain.tools]\nrustc = \"rustc\"\n\n\
         # One direct `rustc` call on the crate root. Every .rs file is declared\n\
         # as an input so editing any module invalidates this action; rustc still\n\
         # decides which of them it actually reads.\n\
         [target.{name}]\nkind = \"command\"\ntool = \"rustc\"\n\
         args = [\"--edition\", \"2021\", {entry:?}, \"-o\", \"${{out}}\"]\n\
         inputs = {}\noutputs = {}\nsandbox = false\n",
        toml_array(&sources),
        toml_array(std::slice::from_ref(&output)),
    );
    Ok(Scaffold { manifest, summary })
}

fn scaffold_go(root: &Path) -> Result<Scaffold> {
    let sources: Vec<String> = sorted_sources(root, &GO_EXTENSIONS, "Go")?
        .into_iter()
        .filter(|path| !path.ends_with("_test.go"))
        .collect();
    let Some(entry) = sources
        .iter()
        .find(|path| {
            let file = root.join(path);
            contains_declaration(&file, "func main(") && contains_declaration(&file, "package main")
        })
        .cloned()
    else {
        bail!(
            "no `func main` in a `package main` file under {}: init cannot name the \
             artifact of a library-only module",
            root.display()
        )
    };

    let name = workspace_name(root);
    let output = format!(".frost/out/${{config}}/{name}");
    let mut summary = vec![
        format!("{} Go source file(s)", sources.len()),
        format!("entry point: {entry}"),
    ];

    // With a module, `go build` addresses the package; without one it only
    // accepts the file list of that package.
    let package_dir = entry.rsplit_once('/').map(|(dir, _)| dir.to_string());
    let mut args = vec!["build".to_string(), "-o".to_string(), "${out}".to_string()];
    if root.join("go.mod").is_file() {
        let package = match &package_dir {
            Some(dir) => format!("./{dir}"),
            None => ".".to_string(),
        };
        summary.push(format!("go.mod found; building package {package}"));
        args.push(package);
    } else {
        let siblings: Vec<String> = sources
            .iter()
            .filter(|path| path.rsplit_once('/').map(|(dir, _)| dir.to_string()) == package_dir)
            .filter(|path| contains_declaration(&root.join(path), "package main"))
            .cloned()
            .collect();
        summary.push(format!(
            "no go.mod; building {} file(s) of package main directly",
            siblings.len()
        ));
        args.extend(siblings);
    }

    let manifest = format!(
        "# Generated by `frost init` after detecting Go sources and one package main.\n\
         # Next: frost build\n\
         # TODO: review whether the Go module graph belongs behind a command boundary.\n\
         [workspace]\ndefault_targets = [\"{name}\"]\n\n\
         [toolchain.tools]\ngo = \"go\"\n\n\
         # `go build` keeps its own build cache under HOME, which Frost passes\n\
         # through; the action key still covers the declared sources and the\n\
         # `go` binary itself.\n\
         [target.{name}]\nkind = \"command\"\ntool = \"go\"\n\
         args = {}\ninputs = {}\noutputs = {}\nsandbox = false\n",
        toml_array(&args),
        toml_array(&sources),
        toml_array(std::slice::from_ref(&output)),
    );
    Ok(Scaffold { manifest, summary })
}

fn scaffold_typescript(root: &Path) -> Result<Scaffold> {
    let sources = sorted_sources(root, &TYPESCRIPT_EXTENSIONS, "TypeScript")?;
    let name = workspace_name(root);
    let output_dir = "dist/${config}".to_string();
    let mut summary = vec![format!("{} TypeScript source file(s)", sources.len())];

    // `tsc` names its outputs after the module graph, so Frost owns the whole
    // directory instead of a file list it cannot write down in advance.
    let mut args = Vec::new();
    if root.join("tsconfig.json").is_file() {
        summary.push("tsconfig.json drives the compile; --outDir is overridden by Frost".into());
        args.extend([
            "-p".to_string(),
            "tsconfig.json".to_string(),
            "--outDir".to_string(),
            "${output_dir}".to_string(),
        ]);
    } else {
        summary.push("no tsconfig.json; compiler options are written into the manifest".into());
        args.extend([
            "--outDir".to_string(),
            "${output_dir}".to_string(),
            "--module".to_string(),
            "es2022".to_string(),
            "--target".to_string(),
            "es2022".to_string(),
            "--moduleResolution".to_string(),
            "bundler".to_string(),
            "${in}".to_string(),
        ]);
    }
    summary.push("`tsc` must be on PATH; check with `frost doctor`".into());

    let mut inputs = sources.clone();
    if root.join("tsconfig.json").is_file() {
        inputs.push("tsconfig.json".to_string());
        inputs.sort();
    }
    let manifest = format!(
        "# Generated by `frost init` after detecting TypeScript sources.\n\
         # Next: frost build\n\
         # TODO: review the compiler options and Frost-owned output directory.\n\
         [workspace]\ndefault_targets = [\"{name}\"]\n\n\
         [toolchain.tools]\ntsc = \"tsc\"\n\n\
         [target.{name}]\nkind = \"command\"\ntool = \"tsc\"\n\
         args = {}\ninputs = {}\noutput_dirs = [{output_dir:?}]\nsandbox = false\n",
        toml_array(&args),
        toml_array(&inputs),
    );
    Ok(Scaffold { manifest, summary })
}

fn scaffold_python(root: &Path) -> Result<Scaffold> {
    let sources = sorted_sources(root, &PYTHON_EXTENSIONS, "Python")?;
    let pyproject = std::fs::read_to_string(root.join("pyproject.toml")).unwrap_or_default();
    let distribution = pyproject_field(&pyproject, "name").unwrap_or_else(|| workspace_name(root));
    let version = pyproject_field(&pyproject, "version").unwrap_or_else(|| "0.1.0".to_string());
    if !version
        .split('.')
        .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
    {
        bail!(
            "pyproject.toml version {version:?} is not a normalized numeric release, which \
             `frost pack-wheel` requires for a standards-compliant wheel filename. set a \
             numeric version, or declare the packaging step as its own command target"
        );
    }

    // The wheel installs the contents of its input root into purelib, so the
    // root must be the directory that holds the importable package.
    let package_root = if root.join("src").is_dir() && sources.iter().any(|s| s.starts_with("src/"))
    {
        "src".to_string()
    } else {
        match sources
            .iter()
            .find(|path| path.ends_with("__init__.py"))
            .and_then(|path| path.rsplit_once('/'))
            .and_then(|(dir, _)| dir.rsplit_once('/').map(|(parent, _)| parent.to_string()))
        {
            Some(parent) => parent,
            None => ".".to_string(),
        }
    };
    let inputs: Vec<String> = sources
        .iter()
        .filter(|path| package_root == "." || path.starts_with(&format!("{package_root}/")))
        .cloned()
        .collect();
    if inputs.is_empty() {
        bail!(
            "no Python sources under the detected package root {package_root:?}; declare the \
             packaging step as its own command target"
        );
    }

    let normalized = distribution
        .to_ascii_lowercase()
        .replace(['-', '.'], "_")
        .replace(' ', "_");
    let output = format!(".frost/out/${{config}}/{normalized}-{version}-py3-none-any.whl");
    let summary = vec![
        format!("{} Python source file(s)", inputs.len()),
        format!("distribution {distribution} {version}, packed from {package_root}/"),
        "a deterministic wheel is the artifact; the interpreter and installer stay yours".into(),
    ];
    let manifest = format!(
        "# Generated by `frost init` after detecting a pure-Python source tree.\n\
         # Next: frost build\n\
         # TODO: review the inferred distribution, version and package root.\n\
         [workspace]\ndefault_targets = [\"{normalized}\"]\n\n\
         [toolchain.tools]\nfrost = \"frost\"\n\n\
         # `frost pack-wheel` writes a deterministic, standards-compliant pure-Python\n\
         # wheel: no interpreter runs, so the action is reproducible by construction.\n\
         [target.{normalized}]\nkind = \"command\"\ntool = \"frost\"\n\
         args = [\"pack-wheel\", \"--input\", {package_root:?}, \"--distribution\", \
         {distribution:?}, \"--version\", {version:?}, \"--output\", \"${{out}}\"]\n\
         inputs = {}\noutputs = {}\nsandbox = false\n",
        toml_array(&inputs),
        toml_array(std::slice::from_ref(&output)),
    );
    Ok(Scaffold { manifest, summary })
}

fn sorted_sources(root: &Path, extensions: &[&str], label: &str) -> Result<Vec<String>> {
    let mut sources = Vec::new();
    collect_sources(root, root, &mut sources, 0, extensions)?;
    sources.sort();
    if sources.is_empty() {
        bail!(
            "no {label} sources under {}; choose another --language or add a direct \
             command target",
            root.display()
        );
    }
    Ok(sources)
}

fn workspace_name(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .map(sanitize_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "app".to_string())
}

/// Textual declaration probe shared by the non-native scaffolds. Comment lines
/// are skipped; anything subtler belongs in the file the author reads.
fn contains_declaration(path: &Path, needle: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    text.lines()
        .map(str::trim_start)
        .filter(|line| !line.starts_with("//") && !line.starts_with('#') && !line.starts_with('*'))
        .any(|line| line.contains(needle))
}

/// First `key = "value"` of the `[project]` table. Deliberately not a full TOML
/// parse: a miss falls back to a default the summary prints.
fn pyproject_field(text: &str, key: &str) -> Option<String> {
    let mut in_project = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_project = line == "[project]";
            continue;
        }
        if !in_project {
            continue;
        }
        if let Some(rest) = line.strip_prefix(key) {
            if let Some(value) = rest.trim_start().strip_prefix('=') {
                let value = value.trim().trim_matches('"').trim_matches('\'');
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

fn collect_sources(
    root: &Path,
    dir: &Path,
    out: &mut Vec<String>,
    depth: usize,
    extensions: &[&str],
) -> Result<()> {
    if depth > 8 {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.')
            || matches!(
                name.as_ref(),
                "target"
                    | "build"
                    | "node_modules"
                    | "dist"
                    | "vendor"
                    | "venv"
                    | "__pycache__"
                    | "site-packages"
            )
        {
            continue;
        }
        let ty = entry.file_type()?;
        let path = entry.path();
        if ty.is_dir() && !ty.is_symlink() {
            collect_sources(root, &path, out, depth + 1, extensions)?;
        } else if ty.is_file()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|extension| extensions.contains(&extension))
        {
            if let Ok(relative) = path.strip_prefix(root) {
                out.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    Ok(())
}

fn collect_named_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<String>,
    depth: usize,
    names: &[&str],
) -> Result<()> {
    if depth > 8 {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.')
            || matches!(
                name.as_ref(),
                "target"
                    | "build"
                    | "node_modules"
                    | "dist"
                    | "vendor"
                    | "venv"
                    | "__pycache__"
                    | "site-packages"
            )
        {
            continue;
        }
        let ty = entry.file_type()?;
        let path = entry.path();
        if ty.is_dir() && !ty.is_symlink() {
            collect_named_files(root, &path, out, depth + 1, names)?;
        } else if ty.is_file() && names.contains(&name.as_ref()) {
            if let Ok(relative) = path.strip_prefix(root) {
                out.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    Ok(())
}

fn java_main_class(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let has_main = text.lines().map(str::trim).any(|line| {
        !line.starts_with("//")
            && (line.contains("static void main(") || line.contains("static void main ("))
    });
    if !has_main {
        return None;
    }
    let class = path.file_stem()?.to_str()?;
    let package = text.lines().map(str::trim).find_map(|line| {
        let package = line.strip_prefix("package ")?.strip_suffix(';')?.trim();
        (!package.is_empty()
            && package
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "._$".contains(character)))
        .then_some(package)
    });
    Some(match package {
        Some(package) => format!("{package}.{class}"),
        None => class.to_string(),
    })
}

/// Whether a file looks like it defines `main`. A scaffold is allowed to be
/// wrong here — the author reads the file it wrote — so this stays a textual
/// check rather than pulling in a parser.
fn defines_main(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    text.lines()
        .map(str::trim_start)
        .filter(|line| !line.starts_with("//") && !line.starts_with('*'))
        .any(|line| line.contains("main(") && (line.contains("int ") || line.starts_with("main(")))
}

fn sanitize_name(raw: &str) -> String {
    let name: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    name.trim_matches('-').to_string()
}

fn toml_array(values: &[String]) -> String {
    let items: Vec<String> = values.iter().map(|v| format!("{v:?}")).collect();
    format!("[{}]", items.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = r#"
        [workspace]
        default_targets = ["app"]

        [toolchain]
        cc = "gcc"
        cflags = ["-O2"]

        [target.util]
        kind = "cc_library"
        srcs = ["src/util.c"]
        includes = ["include"]

        [target.app]
        kind = "cc_binary"
        srcs = ["src/main.c"]
        deps = ["util"]

        [target.gen]
        kind = "genrule"
        cmd = "sh gen.sh ${out}"
        inputs = ["gen.sh"]
        outputs = ["gen/config.h"]
    "#;

    #[test]
    fn parses_valid_manifest() {
        let m = Manifest::parse_str(OK).unwrap();
        assert_eq!(m.default_targets, vec!["app"]);
        assert_eq!(m.toolchain.cc, "gcc");
        assert_eq!(m.targets.len(), 3);
        assert_eq!(m.targets["app"].deps, vec!["util"]);
    }

    #[test]
    fn rejects_unknown_dep() {
        let text = r#"
            [target.app]
            kind = "cc_binary"
            srcs = ["a.c"]
            deps = ["nope"]
        "#;
        let err = Manifest::parse_str(text).unwrap_err().to_string();
        assert!(err.contains("unknown dep"), "{err}");
    }

    #[test]
    fn rejects_self_dep() {
        let text = r#"
            [target.app]
            kind = "cc_binary"
            srcs = ["a.c"]
            deps = ["app"]
        "#;
        let err = Manifest::parse_str(text).unwrap_err().to_string();
        assert!(err.contains("depends on itself"), "{err}");
    }

    #[test]
    fn rejects_absolute_src() {
        let text = r#"
            [target.app]
            kind = "cc_binary"
            srcs = ["/etc/passwd"]
        "#;
        assert!(Manifest::parse_str(text).is_err());
    }

    #[test]
    fn rejects_genrule_without_outputs() {
        let text = r#"
            [target.g]
            kind = "genrule"
            cmd = "true"
        "#;
        assert!(Manifest::parse_str(text).is_err());
    }

    #[test]
    fn command_targets_require_config_isolation_and_named_tools() {
        let valid = r#"
            [toolchain.tools]
            javac = "javac"

            [platform.device.tools]
            javac = "device-javac"

            [target.java]
            kind = "command"
            tool = "javac"
            args = ["-d", "${out_dir}", "${in}"]
            inputs = ["src/Hello.java"]
            outputs = [".frost/out/${config}/java/Hello.class"]
            env = { RELEASE = "1" }
            pass_env = ["JAVA_HOME"]
            clean_dirs = [".frost/tmp/${config}/java"]
            preserve_outputs = true
            steps = [{ tool = "javac", args = ["--version"] }]
            sandbox = false
        "#;
        let manifest = Manifest::parse_str(valid).unwrap();
        let target = &manifest.targets["java"];
        assert_eq!(target.kind, TargetKind::Command);
        assert_eq!(target.tool.as_deref(), Some("javac"));
        assert_eq!(target.pass_env, vec!["JAVA_HOME"]);
        assert_eq!(target.steps[0].tool, "javac");
        assert_eq!(target.clean_dirs, vec![".frost/tmp/${config}/java"]);
        assert!(target.preserve_outputs);
        assert_eq!(manifest.toolchain.tools["javac"], "javac");
        assert_eq!(
            manifest.toolchain_for("device").unwrap().tools["javac"],
            "device-javac"
        );

        for invalid in [
            valid.replace("${config}/", ""),
            valid.replace("tool = \"javac\"", ""),
            valid.replace("pass_env = [\"JAVA_HOME\"]", "pass_env = [\"BAD=NAME\"]"),
            valid.replace(
                "clean_dirs = [\".frost/tmp/${config}/java\"]",
                "clean_dirs = [\".frost/tmp/java\"]",
            ),
        ] {
            assert!(
                Manifest::parse_str(&invalid).is_err(),
                "invalid command target was accepted:\n{invalid}"
            );
        }
    }

    #[test]
    fn depfile_format_selects_where_the_dependency_report_comes_from() {
        let manifest = |extra: &str| {
            format!(
                r#"
                [toolchain.tools]
                cl = "cl.exe"

                [target.obj]
                kind = "command"
                tool = "cl"
                args = ["/c", "${{in}}"]
                inputs = ["src/main.c"]
                outputs = [".frost/out/${{config}}/main.obj"]
                {extra}
                "#
            )
        };
        let make = Manifest::parse_str(&manifest(
            "depfile = \".frost/out/${config}/main.d\"\ndepfile_format = \"make\"",
        ))
        .unwrap();
        assert_eq!(
            make.targets["obj"].depfile_format,
            crate::depfile::Format::Make
        );

        // MSVC writes its includes to stdout, so this format takes no path.
        let showincludes =
            Manifest::parse_str(&manifest("depfile_format = \"showincludes\"")).unwrap();
        assert_eq!(
            showincludes.targets["obj"].depfile_format,
            crate::depfile::Format::ShowIncludes
        );
        assert!(showincludes.targets["obj"].depfile.is_none());

        for invalid in [
            "depfile_format = \"clang-scan-deps\"",
            "depfile_format = \"showincludes\"\ndepfile = \".frost/out/${config}/main.d\"",
            "depfile_format = \"lines\"",
        ] {
            assert!(
                Manifest::parse_str(&manifest(invalid)).is_err(),
                "invalid depfile_format was accepted: {invalid}"
            );
        }
    }

    #[test]
    fn command_targets_may_own_output_directories_instead_of_naming_files() {
        // A bundler names its outputs after their content, so the file list
        // cannot be written down in advance. Declaring the directory is the
        // only honest description of what the action produces.
        let valid = r#"
            [toolchain.tools]
            npm = "npm"

            [target.web]
            kind = "command"
            tool = "npm"
            args = ["run", "build"]
            inputs = ["src/**/*.ts", "package.json"]
            output_dirs = ["dist/${config}"]
        "#;
        let manifest = Manifest::parse_str(valid).unwrap();
        let target = &manifest.targets["web"];
        assert!(target.outputs.is_empty(), "no file needs to be named");
        assert_eq!(target.output_dirs, vec!["dist/${config}"]);

        for invalid in [
            // Without ${config} two configurations would publish into one tree.
            valid.replace("dist/${config}", "dist"),
            // Ownership of the same bytes must not be claimed twice.
            valid.replace(
                "output_dirs = [\"dist/${config}\"]",
                "output_dirs = [\"dist/${config}\", \"dist/${config}/assets\"]",
            ),
            format!("{valid}\noutputs = [\"dist/${{config}}/index.html\"]"),
            valid.replace(
                "output_dirs = [\"dist/${config}\"]",
                "output_dirs = [\"dist/${config}\"]\nclean_dirs = [\"dist/${config}/tmp\"]",
            ),
            // Only command targets own directories.
            valid.replace("kind = \"command\"", "kind = \"genrule\"") + "\ncmd = \"true\"\n",
        ] {
            assert!(
                Manifest::parse_str(&invalid).is_err(),
                "invalid output_dirs target was accepted:\n{invalid}"
            );
        }
    }

    #[test]
    fn tests_accept_exactly_one_shell_or_direct_argv_contract() {
        let direct = Manifest::parse_str(
            r#"
            [toolchain.tools]
            pytest = "python3"

            [target.unit]
            kind = "test"
            tool = "pytest"
            args = ["-m", "pytest", "tests/unit.py"]
            inputs = ["src/**/*.py", "tests/unit.py"]
            env = { PYTHONHASHSEED = "0" }
            pass_env = ["PYTHONPATH"]
            sandbox = false
            "#,
        )
        .unwrap();
        assert_eq!(direct.targets["unit"].tool.as_deref(), Some("pytest"));
        assert_eq!(direct.targets["unit"].pass_env, ["PYTHONPATH"]);

        for invalid in [
            "[target.t]\nkind='test'\n",
            "[target.t]\nkind='test'\ncmd='true'\ntool='runner'\n",
            "[target.t]\nkind='test'\ntool='runner'\noutputs=['out']\n",
            "[target.t]\nkind='test'\ntool='runner'\nsteps=[{tool='runner',args=[]}]\n",
        ] {
            assert!(
                Manifest::parse_str(invalid).is_err(),
                "invalid test target was accepted: {invalid}"
            );
        }
    }

    #[test]
    fn rejects_unknown_field() {
        let text = r#"
            [target.app]
            kind = "cc_binary"
            srcs = ["a.c"]
            cost_ms = 30
        "#;
        assert!(Manifest::parse_str(text).is_err());
    }

    #[test]
    fn parses_kofun_binary_and_rejects_unsupported_shapes() {
        let text = r#"
            [toolchain]
            kofunc = "tools/kofun"

            [target.app]
            kind = "kofun_binary"
            srcs = ["src/main.kofun"]
        "#;
        let manifest = Manifest::parse_str(text).unwrap();
        assert_eq!(manifest.toolchain.kofunc.as_deref(), Some("tools/kofun"));
        assert_eq!(manifest.targets["app"].kind, TargetKind::KofunBinary);
        assert_eq!(manifest.default_targets, vec!["app"]);

        for invalid in [
            "[target.app]\nkind='kofun_binary'\nsrcs=[]\n",
            "[target.app]\nkind='kofun_binary'\nsrcs=['a.kofun','b.kofun']\n",
            "[target.app]\nkind='kofun_binary'\nsrcs=['main.kf']\n",
            "[target.app]\nkind='kofun_binary'\nsrcs=['main.kofun']\ncflags=['-O2']\n",
        ] {
            assert!(
                Manifest::parse_str(invalid).is_err(),
                "invalid Kofun target was accepted: {invalid}"
            );
        }
    }

    #[test]
    fn platform_overlay_resolves_toolchain() {
        let text = r#"
            [toolchain]
            cc = "gcc"
            cxx = "g++"
            kofunc = "host-kofun"
            cflags = ["-O2"]

            [platform.aarch64]
            cc = "aarch64-linux-gnu-gcc"
            kofunc = "aarch64-kofun"
            sysroot = "sysroots/aarch64"
            cflags = ["-mcpu=cortex-a53"]
            ldflags = ["-static"]

            [target.app]
            kind = "cc_binary"
            srcs = ["a.c"]
        "#;
        let m = Manifest::parse_str(text).unwrap();

        let host = m.toolchain_for(HOST_PLATFORM).unwrap();
        assert_eq!(host.cc, "gcc");
        assert_eq!(host.kofunc.as_deref(), Some("host-kofun"));
        assert_eq!(host.cflags, vec!["-O2"]);
        assert_eq!(host.arflags, default_arflags());

        let cross = m.toolchain_for("aarch64").unwrap();
        assert_eq!(cross.cc, "aarch64-linux-gnu-gcc");
        assert_eq!(cross.kofunc.as_deref(), Some("aarch64-kofun"));
        assert_eq!(cross.cxx, "g++", "unset drivers inherit the root toolchain");
        assert_eq!(
            cross.cflags,
            vec!["-O2", "--sysroot=sysroots/aarch64", "-mcpu=cortex-a53"]
        );
        assert_eq!(cross.ldflags, vec!["--sysroot=sysroots/aarch64", "-static"]);
    }

    #[test]
    fn unknown_platform_errors_with_candidates() {
        let m = Manifest::parse_str(
            r#"
            [platform.rv64]
            cc = "riscv64-elf-gcc"

            [target.app]
            kind = "cc_binary"
            srcs = ["a.c"]
            "#,
        )
        .unwrap();
        let err = m.toolchain_for("nope").unwrap_err().to_string();
        assert!(err.contains("unknown platform"), "{err}");
        assert!(err.contains("rv64"), "{err}");
    }

    #[test]
    fn suggests_only_genuinely_close_names() {
        assert_eq!(closest("relase", ["debug", "release"]), Some("release"));
        assert_eq!(closest("aarch65", ["aarch64", "riscv"]), Some("aarch64"));
        assert_eq!(closest("ap", ["app", "lib"]), Some("app"));
        // A short name that resembles nothing gets no suggestion: a wrong
        // hint sends the reader down the wrong path.
        assert_eq!(closest("zzz", ["debug", "release"]), None);
        assert_eq!(closest("windows", ["aarch64"]), None);
        assert_eq!(closest("anything", []), None);
    }

    #[test]
    fn rejects_reserved_host_platform() {
        let text = r#"
            [platform.host]
            cc = "gcc"

            [target.app]
            kind = "cc_binary"
            srcs = ["a.c"]
        "#;
        let err = Manifest::parse_str(text).unwrap_err().to_string();
        assert!(err.contains("reserved"), "{err}");
    }

    #[test]
    fn default_targets_fall_back_to_binaries() {
        let text = r#"
            [target.lib]
            kind = "cc_library"
            srcs = ["l.c"]

            [target.tool]
            kind = "cc_binary"
            srcs = ["t.c"]
        "#;
        let m = Manifest::parse_str(text).unwrap();
        assert_eq!(m.default_targets, vec!["tool"]);
    }

    #[test]
    fn java_scaffold_is_a_parseable_deterministic_jar_pipeline() {
        let root =
            std::env::temp_dir().join(format!("frost-core-java-scaffold-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src/main/java/com/example")).unwrap();
        std::fs::write(
            root.join("src/main/java/com/example/App.java"),
            "package com.example;\n\
             public final class App {\n\
               public static void main(String[] args) {\n\
                 System.out.println(\"java-init-ok\");\n\
               }\n\
             }\n",
        )
        .unwrap();

        let scaffold = scaffold(&root).unwrap();
        assert!(
            scaffold
                .summary
                .iter()
                .any(|line| line == "entry point: com.example.App"),
            "{:?}",
            scaffold.summary
        );
        let manifest = Manifest::parse_str(&scaffold.manifest).unwrap();
        let target_name = root.file_name().unwrap().to_str().unwrap();
        let target = &manifest.targets[target_name];
        assert_eq!(target.kind, TargetKind::Command);
        assert_eq!(target.tool.as_deref(), Some("javac"));
        assert_eq!(
            target.outputs,
            [format!(".frost/out/${{config}}/{target_name}.jar")]
        );
        assert_eq!(target.steps.len(), 1);
        assert_eq!(target.steps[0].tool, "frost");
        assert_eq!(
            target.steps[0].args,
            [
                "pack-jar",
                "--input",
                "${clean_dir}",
                "--output",
                "${out}",
                "--main-class",
                "com.example.App",
            ]
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn mixed_native_and_java_scaffold_requires_an_explicit_language() {
        let root =
            std::env::temp_dir().join(format!("frost-core-mixed-scaffold-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.c"), "int main(void) { return 0; }\n").unwrap();
        std::fs::write(
            root.join("src/App.java"),
            "public final class App { public static void main(String[] args) {} }\n",
        )
        .unwrap();

        let error = scaffold(&root).unwrap_err().to_string();
        assert!(error.contains("--language native"), "{error}");
        assert!(error.contains("--language java"), "{error}");
        let explicit = scaffold_for(&root, ScaffoldLanguage::Java).unwrap();
        assert!(explicit.manifest.contains("inputs = [\"src/App.java\"]"));
        assert!(!explicit.manifest.contains("src/main.c"));
        Manifest::parse_str(&explicit.manifest).unwrap();

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn java_scaffold_does_not_silently_bypass_gradle_or_maven() {
        for marker in ["pom.xml", "build.gradle.kts"] {
            let root = std::env::temp_dir().join(format!(
                "frost-core-java-project-scaffold-{}-{}",
                marker.replace('.', "-"),
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("src/main/java")).unwrap();
            std::fs::write(
                root.join("src/main/java/App.java"),
                "public final class App { public static void main(String[] args) {} }\n",
            )
            .unwrap();
            std::fs::write(root.join(marker), "project configuration\n").unwrap();

            let error = scaffold(&root).unwrap_err().to_string();
            assert!(error.contains(marker), "{error}");
            assert!(error.contains("kind = \"command\""), "{error}");
            let explicit = scaffold_for(&root, ScaffoldLanguage::Java).unwrap();
            Manifest::parse_str(&explicit.manifest).unwrap();

            let _ = std::fs::remove_dir_all(&root);
        }
    }

    fn scaffold_fixture(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "frost-core-scaffold-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        root
    }

    #[test]
    fn rust_scaffold_drives_rustc_directly_and_declares_every_module() {
        let root = scaffold_fixture("rust");
        std::fs::write(
            root.join("src/main.rs"),
            "mod helper;\nfn main() { println!(\"{}\", helper::value()); }\n",
        )
        .unwrap();
        std::fs::write(root.join("src/helper.rs"), "pub fn value() -> i32 { 42 }\n").unwrap();

        let scaffold = scaffold(&root).unwrap();
        assert!(
            scaffold
                .summary
                .iter()
                .any(|line| line == "entry point: src/main.rs"),
            "{:?}",
            scaffold.summary
        );
        let manifest = Manifest::parse_str(&scaffold.manifest).unwrap();
        let name = root.file_name().unwrap().to_str().unwrap();
        let target = &manifest.targets[name];
        assert_eq!(target.kind, TargetKind::Command);
        assert_eq!(target.tool.as_deref(), Some("rustc"));
        assert_eq!(
            target.args,
            ["--edition", "2021", "src/main.rs", "-o", "${out}"]
        );
        // A module the crate root pulls in must invalidate the action even
        // though rustc, not Frost, decides that it is read.
        assert_eq!(target.inputs, ["src/helper.rs", "src/main.rs"]);
        assert_eq!(target.outputs, [format!(".frost/out/${{config}}/{name}")]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rust_scaffold_refuses_a_library_only_crate() {
        let root = scaffold_fixture("rust-lib");
        std::fs::write(root.join("src/lib.rs"), "pub fn value() -> i32 { 1 }\n").unwrap();

        let error = scaffold(&root).unwrap_err().to_string();
        assert!(error.contains("no `fn main`"), "{error}");
        assert!(error.contains("kind = \"command\""), "{error}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn go_scaffold_uses_package_mode_only_when_a_module_exists() {
        let root = scaffold_fixture("go");
        std::fs::write(
            root.join("src/main.go"),
            "package main\nimport \"fmt\"\nfunc main() { fmt.Println(\"go\") }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/support.go"),
            "package main\nfunc support() int { return 1 }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/main_test.go"),
            "package main\nimport \"testing\"\nfunc TestX(t *testing.T) {}\n",
        )
        .unwrap();

        // No module: `go build` only accepts the file list of that package,
        // and test files are not part of the binary.
        let file_mode = scaffold(&root).unwrap();
        let manifest = Manifest::parse_str(&file_mode.manifest).unwrap();
        let name = root.file_name().unwrap().to_str().unwrap();
        let target = &manifest.targets[name];
        assert_eq!(
            target.args,
            ["build", "-o", "${out}", "src/main.go", "src/support.go"]
        );
        assert!(!target
            .inputs
            .iter()
            .any(|input| input.ends_with("_test.go")));

        std::fs::write(root.join("go.mod"), "module demo\n\ngo 1.22\n").unwrap();
        let package_mode = scaffold(&root).unwrap();
        let manifest = Manifest::parse_str(&package_mode.manifest).unwrap();
        assert_eq!(
            manifest.targets[name].args,
            ["build", "-o", "${out}", "./src"]
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn typescript_scaffold_owns_the_output_directory() {
        let root = scaffold_fixture("typescript");
        std::fs::write(root.join("src/index.ts"), "export const x: number = 1;\n").unwrap();

        // tsc names its outputs after the module graph, so the directory is
        // the only honest declaration.
        let plain = scaffold(&root).unwrap();
        let manifest = Manifest::parse_str(&plain.manifest).unwrap();
        let name = root.file_name().unwrap().to_str().unwrap();
        let target = &manifest.targets[name];
        assert_eq!(target.tool.as_deref(), Some("tsc"));
        assert_eq!(target.output_dirs, ["dist/${config}"]);
        assert!(target.outputs.is_empty());
        assert!(target.args.contains(&"${output_dir}".to_string()));

        std::fs::write(root.join("tsconfig.json"), "{\"compilerOptions\": {}}\n").unwrap();
        let configured = scaffold(&root).unwrap();
        let manifest = Manifest::parse_str(&configured.manifest).unwrap();
        let target = &manifest.targets[name];
        assert_eq!(
            target.args,
            ["-p", "tsconfig.json", "--outDir", "${output_dir}"]
        );
        // The config decides what is compiled, so it is an input too.
        assert!(target.inputs.contains(&"tsconfig.json".to_string()));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn python_scaffold_packs_a_normalized_deterministic_wheel() {
        let root = scaffold_fixture("python");
        std::fs::create_dir_all(root.join("src/demo_pkg")).unwrap();
        std::fs::write(
            root.join("src/demo_pkg/__init__.py"),
            "def message():\n    return 'ok'\n",
        )
        .unwrap();
        std::fs::write(
            root.join("pyproject.toml"),
            "[project]\nname = \"Demo.Pkg-name\"\nversion = \"1.2.3\"\n",
        )
        .unwrap();

        let scaffold = scaffold(&root).unwrap();
        let manifest = Manifest::parse_str(&scaffold.manifest).unwrap();
        let target = &manifest.targets["demo_pkg_name"];
        assert_eq!(target.tool.as_deref(), Some("frost"));
        assert_eq!(
            target.outputs,
            ["".to_owned() + ".frost/out/${config}/demo_pkg_name-1.2.3-py3-none-any.whl"]
        );
        assert!(target.args.contains(&"pack-wheel".to_string()));
        assert!(target.args.contains(&"Demo.Pkg-name".to_string()));
        assert_eq!(target.inputs, ["src/demo_pkg/__init__.py"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn python_scaffold_refuses_a_version_no_wheel_filename_can_carry() {
        let root = scaffold_fixture("python-version");
        std::fs::create_dir_all(root.join("src/demo")).unwrap();
        std::fs::write(root.join("src/demo/__init__.py"), "x = 1\n").unwrap();
        std::fs::write(
            root.join("pyproject.toml"),
            "[project]\nname = \"demo\"\nversion = \"1.0.0b1\"\n",
        )
        .unwrap();

        let error = scaffold(&root).unwrap_err().to_string();
        assert!(
            error.contains("not a normalized numeric release"),
            "{error}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn auto_detection_refuses_what_a_package_manager_already_owns() {
        // Each case: the marker file, its content, the language that stays
        // available as a deliberate override, and a phrase the message owes
        // the reader.
        let cases: [(&str, &str, &str, ScaffoldLanguage, &str); 6] = [
            (
                "bazel",
                "MODULE.bazel",
                "module(name = \"x\")\n",
                ScaffoldLanguage::Native,
                "frost import-bazel --dry-run",
            ),
            (
                "ninja",
                "build.ninja",
                "rule cc\n  command = cc -c $in -o $out\n",
                ScaffoldLanguage::Native,
                "frost import-ninja build.ninja",
            ),
            (
                "cargo",
                "Cargo.toml",
                "[package]\nname = \"x\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1\"\n",
                ScaffoldLanguage::Rust,
                "Cargo already owns this build",
            ),
            (
                "gomod",
                "go.mod",
                "module x\n\ngo 1.22\n\nrequire github.com/x/y v1.0.0\n",
                ScaffoldLanguage::Go,
                "declares module requirements",
            ),
            (
                "npm",
                "package.json",
                "{\n  \"name\": \"x\",\n  \"dependencies\": { \"left-pad\": \"1.0.0\" }\n}\n",
                ScaffoldLanguage::TypeScript,
                "frost import-npm",
            ),
            (
                "pep621",
                "pyproject.toml",
                "[project]\nname = \"demo\"\nversion = \"1.0.0\"\ndependencies = [\"requests\"]\n",
                ScaffoldLanguage::Python,
                "does not resolve or vendor an environment",
            ),
        ];

        for (label, marker, content, language, expected) in cases {
            let root = scaffold_fixture(label);
            match language {
                ScaffoldLanguage::Native => {
                    std::fs::write(root.join("src/main.c"), "int main(void) { return 0; }\n")
                        .unwrap()
                }
                ScaffoldLanguage::Rust => {
                    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap()
                }
                ScaffoldLanguage::Go => {
                    std::fs::write(root.join("src/main.go"), "package main\nfunc main() {}\n")
                        .unwrap()
                }
                ScaffoldLanguage::TypeScript => {
                    std::fs::write(root.join("src/index.ts"), "export const x = 1;\n").unwrap()
                }
                ScaffoldLanguage::Python => {
                    std::fs::create_dir_all(root.join("src/demo")).unwrap();
                    std::fs::write(root.join("src/demo/__init__.py"), "x = 1\n").unwrap();
                }
                other => panic!("unexpected language {other:?}"),
            }
            std::fs::write(root.join(marker), content).unwrap();

            let error = scaffold(&root).unwrap_err().to_string();
            assert!(error.contains(expected), "{label}: {error}");
            assert!(error.contains(marker), "{label}: {error}");
            assert!(
                error.contains(&format!("--language {}", language.as_str())),
                "{label}: {error}"
            );

            // The override still works: refusing is about guessing, not ability.
            let explicit = scaffold_for(&root, language).unwrap();
            Manifest::parse_str(&explicit.manifest).unwrap();

            let _ = std::fs::remove_dir_all(&root);
        }
    }
}
