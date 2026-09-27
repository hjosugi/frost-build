//! `frost import-dotnet`: turn MSBuild's own evaluation into csc actions.
//!
//! MSBuild can answer "what compiler command line would you use?" without
//! compiling (`-t:Compile -p:SkipCompilerExecution=true
//! -p:ProvideCommandLineArgs=true -getItem:CscCommandLineArgs`). This module
//! asks it once per project, then rewrites that argv into a `kind = "command"`
//! action whose declared inputs are every byte that decides the assembly:
//! the sources, the generated files, the reference assemblies and analyzers
//! the SDK passes, and the bundled Roslyn closure.
//!
//! It never guesses. An argument it cannot classify is an error, because an
//! argument silently dropped is a compiler input missing from the action key.
//! The same reason drives two edges between projects: a dependent compiles
//! against the producer's reference assembly (`/refout`), which changes only
//! when its API changes, so an implementation-only edit does not recompile the
//! world. MSBuild's `ProduceReferenceAssembly` avoidance, as an ordinary
//! content-addressed edge.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const CONFIGURATION: &str = "Release";
/// Where the bundled Roslyn closure and reference assemblies live. One tree for
/// the whole workspace, so the per-action input set does not repeat it.
const SDK_BUNDLE: &str = ".dotnet-sdk";
/// Where generated sources and the shared closure response file land.
const GENERATED_DIR: &str = ".dotnet-gen";
/// The body of the evaluated project files; any change refuses a stale build.
const IMPORT_STAMP: &str = ".dotnet-gen/import-stamp.json";

#[derive(Debug, Deserialize)]
struct MsBuildResult {
    #[serde(rename = "Items", default)]
    items: MsBuildItems,
}

#[derive(Debug, Default, Deserialize)]
struct MsBuildItems {
    #[serde(rename = "CscCommandLineArgs", default)]
    csc: Vec<MsBuildItem>,
}

#[derive(Debug, Deserialize)]
struct MsBuildItem {
    #[serde(rename = "Identity")]
    identity: String,
}

#[derive(Debug, Clone)]
struct DotnetInfo {
    root: PathBuf,
    sdk_version: String,
    roslyn_dir: PathBuf,
}

/// One project's translated action.
struct ProjectActions {
    /// Lowercased project name, matching `[target.<name>]`.
    name: String,
    project: String,
    args: Vec<String>,
    sources: BTreeSet<String>,
    dependencies: Vec<String>,
    generated: Vec<String>,
    is_executable: bool,
}

/// `frost import-dotnet` entry point.
pub(crate) fn run_import(
    root: &Path,
    project: Option<&Path>,
    dotnet: Option<&Path>,
    output: Option<&Path>,
    dry_run: bool,
) -> Result<i32> {
    let root = root
        .canonicalize()
        .with_context(|| format!("resolving workspace {}", root.display()))?;
    let dotnet = resolve_dotnet(dotnet)?;
    let info = dotnet_info(&dotnet)?;
    let projects = discover_projects(&root, project)?;
    if projects.is_empty() {
        bail!("no .csproj found; pass a project or solution path");
    }
    let entry = projects.last().expect("checked non-empty").clone();
    let environment = import_environment(&root);
    run(
        &dotnet,
        &["restore", &relative_to(&root, &entry)?, "-nologo", "-v:q"],
        &root,
        &environment,
    )
    .context("restoring NuGet assets before evaluation")?;

    let mut actions = Vec::new();
    let mut sdk_files: BTreeSet<PathBuf> = BTreeSet::new();
    for project_path in &projects {
        let name = project_name(project_path)?;
        let command = design_time_args(&root, &dotnet, project_path, &name, &environment)?;
        let (args, sources, dependencies, generated, project_sdk_files, is_exe) =
            translate_arguments(&root, &info, project_path, &name, &command)?;
        sdk_files.extend(project_sdk_files);
        actions.push(ProjectActions {
            name: name.to_lowercase(),
            project: name,
            args,
            sources,
            dependencies,
            generated,
            is_executable: is_exe,
        });
    }

    let bundle = bundle_toolchain(&root, &info, &sdk_files)?;
    let entry_name = project_name(&entry)?;
    let entry_is_executable = actions
        .iter()
        .any(|action| action.project == entry_name && action.is_executable);
    let runtime_config = if entry_is_executable {
        Some(generate_runtime_config(
            &root,
            &dotnet,
            &entry,
            &environment,
        )?)
    } else {
        None
    };
    let (evaluation_digest, evaluation_files) = evaluation_digest(&root, &projects)?;
    let manifest = render_manifest(
        &info,
        &actions,
        &entry_name,
        entry_is_executable,
        &evaluation_digest,
        &evaluation_files,
    )?;
    write_generated_files(&root, &actions, &entry_name, runtime_config.as_deref())?;

    if dry_run {
        print!("{manifest}");
    } else {
        let output = output.unwrap_or_else(|| Path::new("frost.toml"));
        let path = root.join(output);
        std::fs::write(&path, &manifest).with_context(|| format!("writing {}", path.display()))?;
        write_stamp(&root, &info.sdk_version, &evaluation_digest)?;
        println!(
            "frost: imported {} project{} · {} SDK files · {}",
            actions.len(),
            if actions.len() == 1 { "" } else { "s" },
            bundle.files,
            path.display()
        );
    }
    Ok(0)
}

/// `frost import-check`: fail a build whose import is stale.
///
/// The generated manifest runs this as the `import_check` action every csc
/// target depends on, so a `.csproj` or `Directory.Build.props` edit after the
/// import fails the build instead of linking stale compiler argv. It is `frost`
/// itself, so no shell or `sha256sum` is needed on any platform.
pub(crate) fn run_check(
    root: &Path,
    digest: &str,
    out: Option<&Path>,
    files: &[PathBuf],
) -> Result<i32> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut hasher = Sha256::new();
    for file in files {
        let path = if file.is_absolute() {
            file.clone()
        } else {
            root.join(file)
        };
        hasher.update(std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?);
    }
    if hex(&hasher.finalize()) != digest {
        eprintln!("frost: the .NET projects changed since frost import-dotnet; re-run it");
        return Ok(1);
    }
    if let Some(out) = out {
        let path = if out.is_absolute() {
            out.to_path_buf()
        } else {
            root.join(out)
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, b"")?;
    }
    Ok(0)
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn import_environment(root: &Path) -> BTreeMap<String, String> {
    let _ = root;
    BTreeMap::from([
        ("DOTNET_CLI_TELEMETRY_OPTOUT".to_string(), "1".to_string()),
        ("DOTNET_NOLOGO".to_string(), "1".to_string()),
        (
            "DOTNET_SKIP_FIRST_TIME_EXPERIENCE".to_string(),
            "1".to_string(),
        ),
    ])
}

fn resolve_dotnet(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        bail!("{} is not a file", path.display());
    }
    if let Ok(configured) = std::env::var("DOTNET_BIN") {
        let path = PathBuf::from(configured);
        if path.is_file() {
            return Ok(path);
        }
    }
    if let Ok(found) = which("dotnet") {
        return Ok(found);
    }
    bail!("dotnet not found; pass --dotnet or set DOTNET_BIN")
}

fn which(name: &str) -> Result<PathBuf> {
    let path = std::env::var_os("PATH").context("PATH is not set")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
        #[cfg(windows)]
        {
            let exe = directory.join(format!("{name}.exe"));
            if exe.is_file() {
                return Ok(exe);
            }
        }
    }
    bail!("{name} not found on PATH")
}

fn dotnet_info(dotnet: &Path) -> Result<DotnetInfo> {
    let dotnet = dotnet
        .canonicalize()
        .with_context(|| format!("resolving {}", dotnet.display()))?;
    let version = run(
        &dotnet,
        &["--version"],
        dotnet.parent().unwrap_or(Path::new(".")),
        &BTreeMap::new(),
    )?
    .trim()
    .to_string();
    let root = dotnet
        .parent()
        .context("dotnet has no installation directory")?
        .to_path_buf();
    let roslyn_dir = root
        .join("sdk")
        .join(&version)
        .join("Roslyn")
        .join("bincore");
    anyhow::ensure!(
        roslyn_dir.join("csc.dll").is_file(),
        "the Roslyn compiler is not at {}; is this a full SDK install?",
        roslyn_dir.display()
    );
    Ok(DotnetInfo {
        root,
        sdk_version: version,
        roslyn_dir,
    })
}

/// The project closure, dependencies before dependents, entry last.
fn discover_projects(root: &Path, project: Option<&Path>) -> Result<Vec<PathBuf>> {
    let entry = match project {
        Some(path) => {
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                root.join(path)
            };
            if path.extension().is_some_and(|ext| ext == "sln") {
                solution_projects(&path)?
            } else if path.is_dir() {
                single_project_in(&path)?
            } else {
                path
            }
        }
        None => {
            let mut found = Vec::new();
            collect_csproj(root, &mut found, 0)?;
            match found.len() {
                0 => bail!("no .csproj under {}", root.display()),
                1 => found.remove(0),
                _ => bail!(
                    "{} projects found; pass the one to import (or a .sln)",
                    found.len()
                ),
            }
        }
    };
    // Depth-first, dependencies first, deduplicated.
    let mut ordered = Vec::new();
    let mut seen = BTreeSet::new();
    visit_project(&entry, &mut ordered, &mut seen)?;
    Ok(ordered)
}

fn visit_project(
    path: &Path,
    ordered: &mut Vec<PathBuf>,
    seen: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("resolving project {}", path.display()))?;
    if !seen.insert(canonical.clone()) {
        return Ok(());
    }
    for reference in project_references(&canonical)? {
        visit_project(&reference, ordered, seen)?;
    }
    ordered.push(canonical);
    Ok(())
}

fn project_references(path: &Path) -> Result<Vec<PathBuf>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let directory = path.parent().unwrap_or(Path::new("."));
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with("<ProjectReference") {
            continue;
        }
        if let Some(start) = line.find("Include=\"") {
            let rest = &line[start + "Include=\"".len()..];
            if let Some(end) = rest.find('"') {
                let relative = rest[..end].replace('\\', "/");
                out.push(directory.join(relative));
            }
        }
    }
    Ok(out)
}

fn collect_csproj(directory: &Path, out: &mut Vec<PathBuf>, depth: usize) -> Result<()> {
    if depth > 8 {
        return Ok(());
    }
    let mut entries: Vec<_> = std::fs::read_dir(directory)
        .with_context(|| format!("reading {}", directory.display()))?
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(name.as_ref(), ".git" | ".frost" | "bin" | "obj" | "target") {
                continue;
            }
            collect_csproj(&path, out, depth + 1)?;
        } else if name.ends_with(".csproj") {
            out.push(path);
        }
    }
    Ok(())
}

fn single_project_in(directory: &Path) -> Result<PathBuf> {
    let mut found = Vec::new();
    collect_csproj(directory, &mut found, 0)?;
    match found.len() {
        0 => bail!("no .csproj under {}", directory.display()),
        1 => Ok(found.remove(0)),
        _ => bail!(
            "{} projects under {}; pass the one to import",
            found.len(),
            directory.display()
        ),
    }
}

fn solution_projects(path: &Path) -> Result<PathBuf> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let directory = path.parent().unwrap_or(Path::new("."));
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("Project(") {
            if let Some(start) = line.find(".csproj\"") {
                // The path is the second quoted field; find it explicitly.
                let fields: Vec<&str> = line.split('"').collect();
                for field in &fields {
                    if field.ends_with(".csproj") {
                        return Ok(directory.join(field.replace('\\', "/")));
                    }
                }
                let _ = start;
            }
        }
    }
    bail!("no .csproj in solution {}", path.display())
}

fn project_name(path: &Path) -> Result<String> {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .with_context(|| format!("{} has no project name", path.display()))
}

/// `dotnet msbuild` config that returns the exact csc argv without compiling.
fn design_time_args(
    root: &Path,
    dotnet: &Path,
    project: &Path,
    name: &str,
    environment: &BTreeMap<String, String>,
) -> Result<Vec<String>> {
    // An up-to-date `CoreCompile` does not run the task at all, so nothing is
    // reported; clear the project's intermediate directory first.
    let intermediate = root.join(".dotnet/obj").join(name).join(CONFIGURATION);
    if intermediate.exists() {
        std::fs::remove_dir_all(&intermediate).ok();
    }
    let project_rel = relative_to(root, project)?;
    let output = run(
        dotnet,
        &[
            "msbuild",
            &project_rel,
            "-nologo",
            "-t:Compile",
            &format!("-p:Configuration={CONFIGURATION}"),
            "-p:BuildProjectReferences=false",
            "-p:SkipCompilerExecution=true",
            "-p:ProvideCommandLineArgs=true",
            "-getItem:CscCommandLineArgs",
        ],
        root,
        environment,
    )?;
    let start = output.find('{').with_context(|| {
        format!(
            "MSBuild returned no item JSON for {name}:\n{}",
            tail(&output)
        )
    })?;
    let parsed: MsBuildResult = serde_json::from_str(&output[start..])
        .with_context(|| format!("MSBuild item JSON for {name} did not parse"))?;
    let args: Vec<String> = parsed
        .items
        .csc
        .into_iter()
        .map(|item| item.identity)
        .collect();
    anyhow::ensure!(
        !args.is_empty(),
        "MSBuild reported no compiler arguments for {name}"
    );
    Ok(args)
}

fn generate_runtime_config(
    root: &Path,
    dotnet: &Path,
    project: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<String> {
    let project_rel = relative_to(root, project)?;
    run(
        dotnet,
        &[
            "msbuild",
            &project_rel,
            "-nologo",
            "-t:ResolveReferences;GenerateBuildRuntimeConfigurationFiles",
            "-p:BuildProjectReferences=false",
        ],
        root,
        environment,
    )?;
    let name = project_name(project)?;
    let bin = root.join(".dotnet/bin").join(&name);
    let mut matches = Vec::new();
    collect_named(&bin, &format!("{name}.runtimeconfig.json"), &mut matches, 0)?;
    anyhow::ensure!(
        matches.len() == 1,
        "expected one generated {name}.runtimeconfig.json, found {}",
        matches.len()
    );
    std::fs::read_to_string(&matches[0]).context("reading the generated runtimeconfig.json")
}

fn collect_named(directory: &Path, name: &str, out: &mut Vec<PathBuf>, depth: usize) -> Result<()> {
    if depth > 8 || !directory.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_named(&path, name, out, depth + 1)?;
        } else if entry.file_name().to_string_lossy() == name {
            out.push(path);
        }
    }
    Ok(())
}

/// Rewrite MSBuild's absolute argv into workspace-relative action inputs.
#[allow(clippy::type_complexity)]
fn translate_arguments(
    root: &Path,
    info: &DotnetInfo,
    project: &Path,
    name: &str,
    arguments: &[String],
) -> Result<(
    Vec<String>,
    BTreeSet<String>,
    Vec<String>,
    Vec<String>,
    BTreeSet<PathBuf>,
    bool,
)> {
    let project_dir = project.parent().unwrap_or(root);
    let obj = root.join(".dotnet/obj");
    let root_n = normalized(root);
    let obj_n = normalized(&obj);
    let info_root_n = normalized(&info.root);
    let mut args = Vec::new();
    let mut sources = BTreeSet::new();
    let mut dependencies = Vec::new();
    let mut generated = Vec::new();
    let mut sdk_files = BTreeSet::new();
    let mut is_executable = false;

    for raw in arguments {
        // MSBuild quotes a value that contains a space, on Windows in
        // particular (`/reference:"C:\Program Files\..."`), and a quoted
        // source path looks the same.
        let argument = raw.trim_matches('"');
        // An absolute source path begins with `/` on POSIX just like an option
        // does; a path that is an existing file is a file. Windows passes
        // `C:\...` absolute paths, quoted when they contain spaces.
        let as_path = Path::new(argument);
        if as_path.is_file() && (argument.starts_with('/') || as_path.is_absolute()) {
            let path = normalized(
                &as_path
                    .canonicalize()
                    .unwrap_or_else(|_| as_path.to_path_buf()),
            );
            if path.starts_with(&obj_n) {
                let relative = generated_relative(name, &path, &mut generated);
                sources.insert(relative.clone());
                args.push(relative);
            } else if path.starts_with(&root_n) {
                let relative = relative_after(&path, &root_n)?
                    .to_string_lossy()
                    .replace('\\', "/");
                sources.insert(relative.clone());
                args.push(relative);
            } else {
                bail!("source {argument:?} of {name} lies outside the workspace");
            }
            continue;
        }
        let (option, value) = split_option(argument);
        let value = value.trim_matches('"');
        match option {
            "/out" | "/refout" => continue,
            "/reference" | "/analyzer" | "/analyzerconfig" => {
                let path = normalized(
                    &Path::new(value)
                        .canonicalize()
                        .unwrap_or_else(|_| PathBuf::from(value)),
                );
                if path.starts_with(&info_root_n) {
                    sdk_files.insert(path.clone());
                    let relative = Path::new(SDK_BUNDLE).join(relative_after(&path, &info_root_n)?);
                    args.push(format!("{option}:{}", path_string(&relative)));
                } else if option == "/reference" && path.starts_with(&obj_n) {
                    let reference = relative_after(&path, &obj_n)?
                        .components()
                        .next()
                        .map(|component| component.as_os_str().to_string_lossy().into_owned())
                        .unwrap_or_default();
                    anyhow::ensure!(
                        path.file_name().is_some_and(
                            |file| file.to_string_lossy() == format!("{reference}.dll")
                        ),
                        "unrecognised project reference {value:?} in {name}"
                    );
                    dependencies.push(reference.clone());
                    args.push(format!(
                        "/reference:.frost/out/${{config}}/api/{reference}.dll"
                    ));
                } else if option == "/analyzerconfig" && path.starts_with(&obj_n) {
                    let relative = generated_relative(name, &path, &mut generated);
                    args.push(format!("/analyzerconfig:{relative}"));
                } else {
                    bail!("cannot place {option} input {value:?} for {name}");
                }
            }
            _ if argument.starts_with('/') => {
                if argument == "/target:exe" || argument == "/target:winexe" {
                    is_executable = true;
                }
                args.push(argument.to_string());
            }
            _ => {
                // A source path relative to the project directory.
                let path = project_dir.join(argument);
                let path = normalized(&path.canonicalize().unwrap_or(path));
                if path.starts_with(&obj_n) {
                    let relative = generated_relative(name, &path, &mut generated);
                    sources.insert(relative.clone());
                    args.push(relative);
                } else if path.starts_with(&root_n) {
                    let relative = path
                        .strip_prefix(root)
                        .expect("checked prefix")
                        .to_string_lossy()
                        .replace('\\', "/");
                    sources.insert(relative.clone());
                    args.push(relative);
                } else {
                    bail!("source {argument:?} of {name} lies outside the workspace");
                }
            }
        }
    }
    Ok((
        args,
        sources,
        dependencies,
        generated,
        sdk_files,
        is_executable,
    ))
}

fn split_option(argument: &str) -> (&str, &str) {
    match argument.split_once(':') {
        Some((option, value)) => (option, value),
        None => (argument, ""),
    }
}

/// Windows `canonicalize` returns a `\\?\` verbatim path while MSBuild prints a
/// plain `C:\…` one, so a lexical prefix comparison between them fails. Strip
/// the prefix before classifying a path; other platforms are unchanged.
fn normalized(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path.to_path_buf(),
    }
}

/// The tail of `path` after `ancestor`, by component count. `Path::strip_prefix`
/// can disagree with `Path::starts_with` on Windows when the two spellings of a
/// prefix differ in case; skipping the same number of components cannot.
fn relative_after(path: &Path, ancestor: &Path) -> Result<PathBuf> {
    let mut components = path.components();
    for _ in ancestor.components() {
        components
            .next()
            .context("path is not under its ancestor")?;
    }
    Ok(components.as_path().to_path_buf())
}

fn generated_relative(project: &str, path: &Path, generated: &mut Vec<String>) -> String {
    let file = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "generated".to_string());
    let relative = format!("{GENERATED_DIR}/{project}/{file}");
    if !generated.contains(&relative) {
        generated.push(relative.clone());
    }
    relative
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn relative_to(root: &Path, path: &Path) -> Result<String> {
    Ok(path
        .strip_prefix(root)
        .with_context(|| format!("{} is outside the workspace", path.display()))?
        .to_string_lossy()
        .replace('\\', "/"))
}

struct Bundle {
    files: usize,
}

/// Hard-link (or copy) the Roslyn closure and every referenced SDK file.
fn bundle_toolchain(root: &Path, info: &DotnetInfo, files: &BTreeSet<PathBuf>) -> Result<Bundle> {
    let bundle = root.join(SDK_BUNDLE);
    let mut count = 0;
    for source in std::iter::once(&info.roslyn_dir)
        .flat_map(|directory| walk_files(directory))
        .chain(files.iter().cloned())
    {
        let Ok(relative) = source.strip_prefix(&info.root) else {
            continue;
        };
        link_or_copy(&source, &bundle.join(relative))?;
        count += 1;
    }
    Ok(Bundle { files: count })
}

fn walk_files(directory: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![directory.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn link_or_copy(source: &Path, destination: &Path) -> Result<()> {
    if destination.exists() {
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::hard_link(source, destination).is_err() {
        std::fs::copy(source, destination)?;
    }
    Ok(())
}

fn render_manifest(
    info: &DotnetInfo,
    actions: &[ProjectActions],
    entry: &str,
    entry_executable: bool,
    evaluation_digest: &str,
    evaluation_files: &[String],
) -> Result<String> {
    let csc = path_string(
        &Path::new(SDK_BUNDLE)
            .join(
                info.roslyn_dir
                    .strip_prefix(&info.root)
                    .expect("roslyn is under the SDK root"),
            )
            .join(csc_name()),
    );
    let mut lines = vec![
        "# Generated by `frost import-dotnet`: MSBuild evaluated each project".to_string(),
        "# once; these are the Roslyn invocations it would have made. Regenerate".to_string(),
        "# after changing any .csproj or Directory.Build.props (the import stamp".to_string(),
        "# records their digest).".to_string(),
        String::new(),
        "[workspace]".to_string(),
        format!("default_targets = [{}]", toml_string(&entry.to_lowercase())),
        String::new(),
        "[toolchain.tools]".to_string(),
        format!("csc = {}", toml_string(&csc)),
        "cp = \"cp\"".to_string(),
        "frost = \"frost\"".to_string(),
        String::new(),
        // A stale import must fail the build rather than link stale argv. This
        // is `frost` itself, so it works the same on every platform.
        "[target.import_check]".to_string(),
        "kind = \"command\"".to_string(),
        "tool = \"frost\"".to_string(),
        {
            let mut args = vec![
                "import-check".to_string(),
                "--digest".to_string(),
                evaluation_digest.to_string(),
                "--out".to_string(),
                "${out}".to_string(),
            ];
            args.extend(evaluation_files.iter().cloned());
            format!("args = {}", toml_array(&args))
        },
        format!("inputs = {}", toml_array(evaluation_files)),
        format!(
            "outputs = {}",
            toml_array(&[".frost/out/${config}/import-check".to_string()])
        ),
        "sandbox = false".to_string(),
        String::new(),
    ];

    // One response file for the SDK reference/analyzer closure every project
    // shares, so it is key material once instead of in every argv.
    let common: Vec<String> = actions
        .first()
        .map(|action| {
            action
                .args
                .iter()
                .filter(|arg| is_closure(arg))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let response_file = format!("{GENERATED_DIR}/sdk-closure.rsp");
    let mut rendered: Vec<(String, Vec<String>, Vec<String>)> = Vec::new();
    for action in actions {
        let closures: Vec<&String> = action.args.iter().filter(|arg| is_closure(arg)).collect();
        let own: Vec<String> = closures.iter().map(|arg| (*arg).clone()).collect();
        if own != common {
            bail!("projects disagree about the SDK reference closure; refusing to share it");
        }
        let response_arg = format!("@{response_file}");
        let mut compacted = Vec::new();
        for argument in &action.args {
            if is_closure(argument) {
                if !compacted.iter().any(|value| value == &response_arg) {
                    compacted.push(response_arg.clone());
                }
            } else {
                compacted.push(argument.clone());
            }
        }
        rendered.push((action.name.clone(), compacted, action.generated.clone()));
    }

    let environment = format!(
        "{{ DOTNET_ROOT = {} }}",
        toml_string(&path_string(&info.root))
    );
    for action in actions {
        let (_name, compacted, mut generated) = rendered
            .iter()
            .find(|(candidate, _, _)| candidate == &action.name)
            .map(|(name, args, generated)| (name.clone(), args.clone(), generated.clone()))
            .expect("every action was rendered");
        if entry_executable && action.project == entry {
            generated.push(format!(
                "{GENERATED_DIR}/{entry}/{entry}.runtimeconfig.json"
            ));
        }
        let mut outputs = vec![format!(".frost/out/${{config}}/bin/{}.dll", action.project)];
        let mut refout = Vec::new();
        if !(entry_executable && action.project == entry) {
            let reference = format!(".frost/out/${{config}}/ref/{}.dll", action.project);
            outputs.push(reference.clone());
            refout.push(format!("/refout:{reference}"));
        }
        let deps: Vec<String> = std::iter::once("import_check".to_string())
            .chain(
                action
                    .dependencies
                    .iter()
                    .map(|dependency| format!("{}_api", dependency.to_lowercase())),
            )
            .collect();
        let mut inputs: Vec<String> = action
            .sources
            .iter()
            .chain(generated.iter())
            .cloned()
            .collect();
        inputs.push(response_file.clone());
        inputs.sort();
        inputs.dedup();
        inputs.push(format!("{SDK_BUNDLE}/**/*"));

        lines.push(format!("[target.{}]", action.name));
        lines.push("kind = \"command\"".to_string());
        lines.push("tool = \"csc\"".to_string());
        lines.push(format!("deps = {}", toml_array(&deps)));
        lines.push(format!("inputs = {}", toml_array(&inputs)));
        lines.push(format!("outputs = {}", toml_array(&outputs)));
        let mut argv = compacted;
        argv.push("/out:${out}".to_string());
        argv.extend(refout);
        lines.push(format!("args = {}", toml_array(&argv)));
        if entry_executable && action.project == entry {
            lines.push(format!(
                "steps = [{{ tool = \"cp\", args = [{}, {}] }}]",
                toml_string(&format!(
                    "{GENERATED_DIR}/{entry}/{entry}.runtimeconfig.json"
                )),
                toml_string(&format!(
                    ".frost/out/${{config}}/bin/{entry}.runtimeconfig.json"
                )),
            ));
            outputs.push(format!(
                ".frost/out/${{config}}/bin/{entry}.runtimeconfig.json"
            ));
        }
        lines.push(format!("env = {environment}"));
        lines.push("sandbox = false".to_string());
        lines.push(String::new());

        if !(entry_executable && action.project == entry) {
            lines.push(format!("[target.{}_api]", action.name));
            lines.push("kind = \"command\"".to_string());
            lines.push("tool = \"cp\"".to_string());
            lines.push(format!("deps = [{}]", toml_string(&action.name)));
            lines.push(format!(
                "args = [{}, \"${{out}}\"]",
                toml_string(&format!(
                    ".frost/out/${{config}}/ref/{}.dll",
                    action.project
                ))
            ));
            lines.push(format!(
                "outputs = [{}]",
                toml_string(&format!(
                    ".frost/out/${{config}}/api/{}.dll",
                    action.project
                ))
            ));
            lines.push("sandbox = false".to_string());
            lines.push(String::new());
        }
    }
    let _ = response_file;
    Ok(lines.join("\n"))
}

fn is_closure(argument: &str) -> bool {
    let option = argument.split(':').next().unwrap_or(argument);
    matches!(option, "/reference" | "/analyzer" | "/analyzerconfig")
        && argument.contains(&format!(":{SDK_BUNDLE}/"))
}

fn csc_name() -> &'static str {
    if cfg!(windows) {
        "csc.exe"
    } else {
        "csc"
    }
}

fn write_generated_files(
    root: &Path,
    actions: &[ProjectActions],
    entry: &str,
    runtime_config: Option<&str>,
) -> Result<()> {
    // Copy generated sources into the workspace so they are declared inputs.
    for action in actions {
        for relative in &action.generated {
            let destination = root.join(relative);
            if destination.exists() {
                continue;
            }
            // The source file is in `.dotnet/obj/<Project>/...`; find it there.
            let name = Path::new(relative)
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_default();
            if let Some(source) = find_generated(root, &action.project, &name) {
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(source, destination)?;
            }
        }
    }
    // The shared closure response file.
    let response = root.join(format!("{GENERATED_DIR}/sdk-closure.rsp"));
    if let Some(parent) = response.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let closure: Vec<String> = actions
        .first()
        .map(|action| {
            action
                .args
                .iter()
                .filter(|arg| is_closure(arg))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    std::fs::write(&response, closure.join("\n") + "\n")?;
    // The entry project's runtimeconfig.json, if it is an executable.
    if let Some(contents) = runtime_config {
        let path = root.join(format!(
            "{GENERATED_DIR}/{entry}/{entry}.runtimeconfig.json"
        ));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents)?;
    }
    Ok(())
}

fn find_generated(root: &Path, project: &str, name: &str) -> Option<PathBuf> {
    let base = root.join(".dotnet/obj").join(project);
    let mut stack = vec![base];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if entry.file_name().to_string_lossy() == name {
                return Some(path);
            }
        }
    }
    None
}

fn evaluation_digest(root: &Path, projects: &[PathBuf]) -> Result<(String, Vec<String>)> {
    let mut paths: Vec<PathBuf> = projects.to_vec();
    let props = root.join("Directory.Build.props");
    if props.is_file() {
        paths.push(props);
    }
    paths.sort();
    let mut hasher = Sha256::new();
    let mut files = Vec::new();
    for path in paths {
        files.push(relative_to(root, &path)?);
        hasher.update(std::fs::read(&path)?);
    }
    let out = hex(&hasher.finalize());
    Ok((out, files))
}

fn write_stamp(root: &Path, sdk_version: &str, digest: &str) -> Result<()> {
    let path = root.join(IMPORT_STAMP);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents = format!(
        "{{\n  \"sdk_version\": {},\n  \"evaluation_inputs_sha256\": {}\n}}\n",
        toml_string(sdk_version),
        toml_string(digest)
    );
    std::fs::write(&path, contents).context("writing the import stamp")
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn toml_array(values: &[String]) -> String {
    if values.is_empty() {
        return "[]".to_string();
    }
    let mut out = String::from("[\n");
    for value in values {
        out.push_str(&format!("  {},\n", toml_string(value)));
    }
    out.push(']');
    out
}

fn run(
    program: &Path,
    args: &[&str],
    cwd: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<String> {
    let mut command = Command::new(program);
    command.args(args).current_dir(cwd);
    for (key, value) in environment {
        command.env(key, value);
    }
    let output = command
        .output()
        .with_context(|| format!("running {} {}", program.display(), args.join(" ")))?;
    if !output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout);
        let err = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{} {} failed with {}:\n{}{}",
            program.display(),
            args.join(" "),
            output.status,
            tail(&text),
            tail(&err)
        );
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(text)
}

fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(40);
    lines[start..].join("\n")
}
