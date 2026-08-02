//! `frost import-ninja`: turn a `build.ninja` into a Frost manifest.

use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;

pub(crate) fn import_ninja(root: &std::path::Path, ninja: PathBuf, output: PathBuf) -> Result<i32> {
    let source = if ninja.is_absolute() {
        ninja
    } else {
        root.join(ninja)
    };
    let text = std::fs::read_to_string(&source)?;
    let mut rules = std::collections::BTreeMap::new();
    let mut current_rule: Option<String> = None;
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("rule ") {
            current_rule = Some(name.trim().to_string());
        } else if let Some(command) = line.trim_start().strip_prefix("command = ") {
            if let Some(name) = &current_rule {
                rules.insert(name.clone(), command.to_string());
            }
        } else if !line.starts_with(' ') {
            current_rule = None;
        }
    }
    let mut generated = String::from("[workspace]\n\n");
    let mut producers = std::collections::BTreeMap::new();
    let mut builds = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("build ") else {
            continue;
        };
        let (outputs, rest) = rest
            .split_once(':')
            .context("invalid Ninja build statement")?;
        let mut fields = rest.split_whitespace();
        let rule = fields.next().context("missing Ninja rule")?;
        let inputs = fields
            .filter(|field| *field != "|")
            .map(str::to_string)
            .collect::<Vec<_>>();
        let outputs = outputs
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let name = sanitize_target(outputs.first().context("build has no output")?);
        for output in &outputs {
            producers.insert(output.clone(), name.clone());
        }
        builds.push((name, rule.to_string(), inputs, outputs));
    }
    for (name, rule, inputs, outputs) in builds {
        let command = rules
            .get(&rule)
            .with_context(|| format!("unsupported/unknown Ninja rule {rule:?}"))?;
        let deps = inputs
            .iter()
            .filter_map(|input| producers.get(input).cloned())
            .collect::<Vec<_>>();
        let files = inputs
            .iter()
            .filter(|input| !producers.contains_key(*input))
            .cloned()
            .collect::<Vec<_>>();
        let expanded = command.replace("$in", "${in}").replace("$out", "${outs}");
        generated.push_str(&format!(
            "[target.{name}]\nkind = \"genrule\"\ncmd = {:?}\n",
            expanded
        ));
        generated.push_str(&format!(
            "inputs = {}\noutputs = {}\ndeps = {}\n\n",
            serde_json::to_string(&files)?,
            serde_json::to_string(&outputs)?,
            serde_json::to_string(&deps)?
        ));
    }
    let destination = if output.is_absolute() {
        output
    } else {
        root.join(output)
    };
    std::fs::write(&destination, generated)?;
    println!("frost: imported {}", source.display());
    Ok(0)
}

fn sanitize_target(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}
