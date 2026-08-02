//! `frost doctor` and `frost info`: what this workspace is configured with, and
//! whether the tools it names are actually there.

use std::path::Path;
use std::path::PathBuf;

use anyhow::bail;
use anyhow::Result;

use crate::frostrc;
use crate::graph::load_graph;
use crate::launch::{find_on_path, is_executable_file};

#[derive(serde::Serialize)]
struct DoctorTool {
    name: String,
    configured: String,
    resolved: Option<String>,
    available: bool,
    required: bool,
}

fn inspect_tool(root: &Path, name: &str, configured: &str, required: bool) -> DoctorTool {
    let selected = PathBuf::from(configured);
    let resolved = if selected.is_absolute() || selected.components().count() > 1 {
        let candidate = if selected.is_absolute() {
            selected
        } else {
            root.join(selected)
        };
        is_executable_file(&candidate).then_some(candidate)
    } else {
        find_on_path(configured)
    };
    DoctorTool {
        name: name.to_string(),
        configured: configured.to_string(),
        available: resolved.is_some(),
        resolved: resolved.map(|path| path.display().to_string()),
        required,
    }
}

/// Every location Frost derives from a configuration, in report order.
///
/// This exists so wrappers, editors and CI scripts ask for a path instead of
/// reimplementing the naming rules — the rules are Frost's to change, the
/// answers are not.
pub(crate) fn info_entries(
    root: &Path,
    profile: &str,
    platform: &str,
) -> Vec<(&'static str, String)> {
    let config = frostbuild_core::paths::config(platform, profile);
    let show = |path: PathBuf| path.display().to_string();
    #[allow(unused_mut)]
    let mut entries = vec![
        ("version", env!("CARGO_PKG_VERSION").to_string()),
        (
            "action_key_schema",
            frostbuild_exec::ACTION_KEY_SCHEMA.to_string(),
        ),
        ("workspace_root", show(root.to_path_buf())),
        (
            "manifest",
            show(root.join(frostbuild_core::manifest::MANIFEST_FILE)),
        ),
        ("config", config.clone()),
        (
            "output_dir",
            show(root.join(format!(".frost/out/{config}"))),
        ),
        (
            "bin_dir",
            show(root.join(format!("{}/{config}", frostbuild_core::graph::BIN_DIR))),
        ),
        (
            "obj_dir",
            show(root.join(format!("{}/{config}", frostbuild_core::graph::OBJ_DIR))),
        ),
        ("tmp_dir", show(root.join(format!(".frost/tmp/{config}")))),
        ("cas_dir", show(root.join(".frost/cas"))),
        (
            "journal",
            show(root.join(frostbuild_core::journal::JOURNAL_REL_PATH)),
        ),
        (
            "hash_cache",
            show(root.join(frostbuild_core::hashcache::CACHE_REL_PATH)),
        ),
        (
            "graph_store",
            show(frostbuild_core::graph_store::store_path(
                root, profile, platform,
            )),
        ),
    ];
    #[cfg(unix)]
    entries.push(("daemon_socket", show(frostbuild_daemon::socket_path(root))));
    entries
}

pub(crate) fn run_info(
    root: &Path,
    key: Option<&str>,
    profile: &str,
    platform: &str,
    json: bool,
) -> Result<i32> {
    let entries = info_entries(root, profile, platform);
    if let Some(key) = key {
        let Some((_, value)) = entries.iter().find(|(name, _)| *name == key) else {
            bail!(
                "unknown info key {key:?}. known keys: {}",
                entries
                    .iter()
                    .map(|(name, _)| *name)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        };
        // A single key prints its bare value so `$(frost info bin_dir)` is
        // directly usable in a script.
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({ key: value }))?
            );
        } else {
            println!("{value}");
        }
        return Ok(0);
    }
    if json {
        let table: serde_json::Map<String, serde_json::Value> = entries
            .into_iter()
            .map(|(name, value)| (name.to_string(), serde_json::Value::String(value)))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::Value::Object(table))?
        );
        return Ok(0);
    }
    let width = entries
        .iter()
        .map(|(name, _)| name.len())
        .max()
        .unwrap_or(0);
    for (name, value) in &entries {
        println!("{name:<width$}  {value}");
    }
    Ok(0)
}

pub(crate) fn run_doctor(
    root: &Path,
    profile: &str,
    platform: &str,
    json: bool,
    frostrc: &frostrc::Resolved,
) -> Result<i32> {
    let graph = load_graph(root, profile, platform)?;
    let mut required = vec![
        inspect_tool(root, "C compiler", &graph.toolchain.cc, true),
        inspect_tool(root, "C++ compiler", &graph.toolchain.cxx, true),
        inspect_tool(root, "archiver", &graph.toolchain.ar, true),
        inspect_tool(root, "shell", frostbuild_core::graph::SHELL, true),
    ];
    if let Some(kofunc) = &graph.toolchain.kofunc {
        required.push(inspect_tool(root, "Kofun compiler", kofunc, true));
    }
    required.extend(
        graph
            .toolchain
            .tools
            .iter()
            .map(|(name, tool)| inspect_tool(root, &format!("tool:{name}"), tool, true)),
    );
    let extras = [
        ("fuzzy target picker", "fzf"),
        ("native debugger (GDB)", "gdb"),
        ("native debugger (LLDB)", "lldb"),
        ("Java debugger", "jdb"),
        ("Java runtime", "java"),
        ("Node runtime/debugger", "node"),
        ("Python runtime/debugger", "python3"),
        ("Linux sandbox", "bwrap"),
        ("Graphviz rendering", "dot"),
    ]
    .into_iter()
    .map(|(name, tool)| inspect_tool(root, name, tool, false))
    .collect::<Vec<_>>();
    let ready = required.iter().all(|tool| tool.available);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "status": if ready { "ready" } else { "blocked" },
                "workspace": root,
                "profile": profile,
                "platform": platform,
                "targets": graph.targets.len(),
                "actions": graph.actions.len(),
                "required_tools": required,
                "optional_integrations": extras,
                "frostrc": frostrc
                    .origins
                    .iter()
                    .map(|origin| serde_json::json!({
                        "file": origin.file,
                        "line": origin.line,
                        "section": origin.section,
                        "key": origin.key,
                    }))
                    .collect::<Vec<_>>(),
            }))?
        );
        return Ok(if ready { 0 } else { 1 });
    }

    println!(
        "frost: doctor · {}",
        if ready { "ready" } else { "action required" }
    );
    println!("|-- workspace  {}", root.display());
    println!("|-- config     {profile} / {platform}");
    // Which file, which section, which key. "A setting is in effect" is not
    // useful without the line to go and change.
    if !frostrc.origins.is_empty() {
        println!(
            "|-- frostrc    {} setting(s) in effect",
            frostrc.origins.len()
        );
        for origin in &frostrc.origins {
            println!(
                "|     {}:{} [{}] {}",
                origin
                    .file
                    .strip_prefix(root)
                    .unwrap_or(&origin.file)
                    .display(),
                origin.line,
                origin.section,
                origin.key
            );
        }
    }
    println!(
        "|-- graph      {} targets / {} actions",
        graph.targets.len(),
        graph.actions.len()
    );
    println!("|-- required tools");
    for (index, tool) in required.iter().enumerate() {
        let branch = if index + 1 == required.len() {
            "|   `--"
        } else {
            "|   |--"
        };
        let location = tool.resolved.as_deref().unwrap_or(&tool.configured);
        println!(
            "{branch} {:<20} {:<7} {location}",
            tool.name,
            if tool.available { "ok" } else { "missing" }
        );
    }
    println!("|-- optional integrations");
    for (index, tool) in extras.iter().enumerate() {
        let branch = if index + 1 == extras.len() {
            "|   `--"
        } else {
            "|   |--"
        };
        println!(
            "{branch} {:<24} {}",
            tool.name,
            if tool.available {
                "available"
            } else {
                "not installed"
            }
        );
    }
    println!(
        "`-- result     {}",
        if ready {
            "build prerequisites are ready"
        } else {
            "install or correct every missing required tool"
        }
    );
    Ok(if ready { 0 } else { 1 })
}
