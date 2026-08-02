//! The action key: the digest that decides whether an action re-runs.
//!
//! Everything that can change what an action produces has to reach this hash,
//! and nothing that cannot may -- a key that misses an input silently reuses a
//! stale result, and a key that includes noise never hits the cache. The
//! length-prefixed encoding is what keeps two different field sets from
//! hashing the same bytes.

use std::borrow::Cow;
use std::collections::BTreeMap;

use crate::ACTION_KEY_SCHEMA;

/// Hash the same length-prefixed payload as `ActionKey::digest`, but feed it
/// directly to BLAKE3. This avoids cloning argv/input maps and allocating a
/// complete canonical string for every cache check.
pub(crate) struct StreamedActionDescriptor<'a> {
    pub(crate) builder: &'a str,
    pub(crate) target: &'a str,
    pub(crate) argv: &'a [String],
    pub(crate) cwd: &'a str,
    pub(crate) toolchain_hash: &'a str,
    /// Directories the action owns entirely. Declaring a different set changes
    /// which bytes the action is answerable for, exactly as changing the
    /// declared output paths does (#64).
    pub(crate) output_dirs: &'a [String],
}

pub(crate) fn streamed_action_key<'a>(
    descriptor: StreamedActionDescriptor<'_>,
    env: &BTreeMap<String, String>,
    inputs: &BTreeMap<String, String>,
    outputs: impl IntoIterator<Item = &'a str>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    update_key_field(&mut hasher, "schema", ACTION_KEY_SCHEMA);
    update_key_field(&mut hasher, "builder", descriptor.builder);
    update_key_field(&mut hasher, "target", descriptor.target);
    update_key_field(&mut hasher, "cwd", descriptor.cwd);
    update_key_field(&mut hasher, "toolchain", descriptor.toolchain_hash);
    for arg in descriptor.argv {
        update_key_field(&mut hasher, "argv", arg);
    }
    for (key, value) in env {
        update_key_field(&mut hasher, "env", key);
        update_key_field(&mut hasher, "env", value);
    }
    for (path, digest) in inputs {
        update_key_field(&mut hasher, "input", path);
        update_key_field(&mut hasher, "input", digest);
    }
    for path in outputs {
        update_key_field(&mut hasher, "output", path);
    }
    for directory in descriptor.output_dirs {
        update_key_field(&mut hasher, "output_dir", directory);
    }
    hasher.finalize().to_hex().to_string()
}

/// Is `path` a file under `directory`? Compared segment-wise so `dist2/a` is
/// not mistaken for a file inside `dist`.
pub(crate) fn path_is_inside(directory: &str, path: &str) -> bool {
    let directory = directory.trim_end_matches('/');
    path.len() > directory.len()
        && path.starts_with(directory)
        && path.as_bytes()[directory.len()] == b'/'
}

pub(crate) fn action_key_argv<'a>(
    action: &'a frostbuild_core::graph::ActionNode,
    stamps: Option<&BTreeMap<String, String>>,
) -> Cow<'a, [String]> {
    if action.followup_argv.is_empty()
        && action.clean_dirs.is_empty()
        && !action.preserve_outputs
        && action.stable_stamps.is_empty()
    {
        return Cow::Borrowed(&action.argv);
    }
    let mut argv = action.argv.clone();
    // The argv itself still holds the unexpanded `${stamp.KEY}`, which is what
    // keeps a *volatile* value out of the key: the reference is constant, only
    // the value changes. A stable value has to get in some other way, so it is
    // named here explicitly.
    for key in &action.stable_stamps {
        argv.push("\0frost-stamp".into());
        argv.push(key.clone());
        argv.push(
            stamps
                .and_then(|stamps| stamps.get(key))
                .cloned()
                .unwrap_or_default(),
        );
    }
    if action.preserve_outputs {
        argv.push("\0frost-preserve-outputs".into());
    }
    for directory in &action.clean_dirs {
        // NUL cannot occur in an OS argument, making these internal boundary
        // tags unambiguous in the canonical key payload.
        argv.push("\0frost-clean-dir".into());
        argv.push(directory.clone());
    }
    for command in &action.followup_argv {
        argv.push("\0frost-next-command".into());
        argv.extend(command.iter().cloned());
    }
    Cow::Owned(argv)
}

fn update_key_field(hasher: &mut blake3::Hasher, key: &str, value: &str) {
    hasher.update(key.as_bytes());
    hasher.update(b"\0");
    let mut digits = [0u8; 20];
    let mut cursor = digits.len();
    let mut length = value.len();
    loop {
        cursor -= 1;
        digits[cursor] = b'0' + (length % 10) as u8;
        length /= 10;
        if length == 0 {
            break;
        }
    }
    hasher.update(&digits[cursor..]);
    hasher.update(b"\0");
    hasher.update(value.as_bytes());
    hasher.update(b"\0");
}
