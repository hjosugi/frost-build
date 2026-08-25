# What the action key covers, and what it does not

frost skips an action when its key matches the journal and its outputs are
intact. Everything that can change what the action produces must therefore
reach the key, or frost will report a build as current that a clean tree
cannot reproduce. That failure is worse than a slow build: it is silent, and
it survives until someone deletes `.frost`.

This is the enumeration. Each row states whether the input reaches the key,
and the reasoning is written down so that a future change to any of them is a
decision rather than an accident. Rows marked **out** are deliberate gaps with
a stated argument; rows marked **gap** are known holes with no defence.

| Input | Reaches the key | How, or why not |
|---|---|---|
| Command line (argv) | yes | Verbatim in the canonical payload. Profile and platform flags arrive this way. |
| Declared input contents | yes | Content digest per path. |
| Discovered input contents | yes | Depfile paths join the key after the first run. |
| **Input file mode** | yes | Mixed into the content digest. `chmod -x` on a script changes no bytes; without this frost reported "up to date" where a clean build failed. |
| Compiler identity | yes | The toolchain fingerprint hashes the resolved `cc`, `cxx` and `ar` binaries, so a PATH switch or a package upgrade invalidates. |
| Named command-tool identity | yes | Every configured `[toolchain.tools]` executable is resolved and hashed. A workspace-relative wrapper is also a declared input of the command action. |
| Command-target static `env` | yes | Merged into the canonical action environment after Frost's baseline. |
| Command-target `pass_env` | yes | The value or absence of every opted-in name joins both the action key and whole-build no-op certificate. Other host variables remain cleared. |
| Command follow-up steps, reset directories and output-preservation mode | yes | Ordered direct argv, configuration-isolated `clean_dirs`, and `preserve_outputs` are tagged into the canonical action payload; every step's named tool is part of the toolchain fingerprint. |
| `CPATH`, `C_INCLUDE_PATH`, `CPLUS_INCLUDE_PATH`, `LIBRARY_PATH`, `SDKROOT`, `MACOSX_DEPLOYMENT_TARGET`, `SystemRoot` | yes | These choose which headers and libraries a compiler finds with an identical command line. |
| Working directory | yes | Recorded relative to the workspace root. |
| Locale | n/a | Forced to `LC_ALL=C` and `LANG=C` for every action, so it cannot vary. |
| Declared output paths | yes | Ordered paths are part of the v3 canonical payload. A changed output set cannot reuse an action result produced for the old set. |
| Output contents | yes, separately | Checked against the journal after the key matches; a modified output re-runs or restores. |
| Stable stamp values (`[stamp]`, `STABLE_*`) | yes | Tagged into the canonical argv beside the value the action was given. The reference in the argv is constant, so the value has to be named separately; a new commit rebuilding the binary that embeds its SHA is the correct answer. |
| Volatile stamp values (`[stamp]`, everything else) | **out** | A wall clock in an action key would rebuild the workspace every second. The action that reads one is re-executed unconditionally instead — one action rather than the graph above it, and early cutoff stops there if the bytes match. A volatile value that reaches a *compile* is rejected when the graph loads, because that turns one unconditional action into a full rebuild. |
| Target `visibility` | **out** | It says who may *ask* for a target, not what building it produces. Two workspaces differing only in visibility build byte-identical outputs and must share cache entries — otherwise declaring a boundary would cost a full rebuild, and nobody would declare one. |
| `PATH` | **out** | Its effect on the compiler is already covered by hashing the resolved drivers. Keying on it would rebuild everything whenever a shell, direnv or CI step exports a different one. |
| `HOME`, `TMPDIR`, `TMP`, `TEMP` | **out** | Scratch locations. An action whose output depends on where its scratch space lives is not hermetic, which is what `--sandbox` and `--check-determinism` exist to surface. |
| Order-only inputs | **out** | By construction: generated headers must exist before a compile, but only the ones actually included should invalidate it, and the depfile names those. |
| Action `timeout` | **out** | A limit is a statement about the environment, not the result. The same inputs under a longer limit must hit the same cache entry, and a build that is reproducible only because it was allowed more seconds is not reproducible. A timed-out action is a failure, and failures are not recorded at all. |
| Action `resources` (`cpu`, `ram_mb`, `exclusive`) and invocation resource limits | **out** | Admission policy changes when identical work starts, not what it produces. A unit regression mutates all three requirements while asserting an identical streamed action key. |
| `--sandbox` | **out** | A checking mode, not a build input. It can only remove access to files an action never declared, so a build that differs under it was already unsound. |
| The host shell frost runs genrules through (`/bin/sh` or `cmd.exe`) | yes | frost picks it, so it sits in the toolchain fingerprint beside the C drivers. The manifest has no way to name it, which is exactly why leaving it out would make it the one tool frost chooses and does not account for. |
| **Other tools a genrule invokes** (`python3`, a script on PATH) | **gap** | frost cannot know which tools a shell command reaches. Declaring the script is not declaring what the script runs. Same class as any undeclared input; `--sandbox` narrows it, nothing closes it. |
| **umask** | **gap** | Affects the permissions of created outputs. Only the executable bit is captured, via the output digest. |
| **Filesystems with coarse mtime** | **gap** | The stat check is (mtime_ns, size ^ mode, inode). Where a filesystem reports whole seconds, a same-size rewrite inside one second can be missed. Not observed on ext4; no cheap defence beyond hashing everything. |

## Restoration is checked too

A key that covers every input still delivers the wrong bytes if what is
restored is not what was stored. The CAS is content-addressed, so an object
that no longer hashes to its own name is corrupt; `materialize` verifies
before publishing, removes the bad object and reports a miss, and the action
re-runs. This was a real hole: flipping one byte in a CAS object produced
`up to date` and a binary that differed from a correct build.

## The rule this table encodes

An input belongs in the key when changing it changes what the action
produces. An input stays out only when one of these holds, and the row must
say which:

1. Something already in the key covers it (PATH, via the driver hashes).
2. Depending on it is already a bug the build model does not admit
   (scratch directories, sandbox visibility).
3. Including it would invalidate constantly for no correctness gain, and the
   risk is documented (there are currently no rows resting only on this).

"It is inconvenient" is not on the list.

## History

Every row marked yes for an environment variable, and the file-mode row, were
gaps found by trying to break the cache rather than by reading the code. The
CPATH case was reproduced end to end: two headers of the same name reachable
only through the environment, an identical command line, and frost handing
back the binary built against the other one. Tests now cover each; they are
named in `crates/frostbuild-cli/tests/e2e.rs` and each one fails against the
engine that preceded its fix.

Declared output paths were also once absent from the key. An unchanged command
could therefore reuse a journal record after the manifest added an output.
Action-key schema v3 keys the declared set, and restoration additionally
requires the recorded output paths to match exactly.

The remaining gaps have no test, because a test would assert the wrong
behaviour. They are written down instead so that the next person to look does
not have to rediscover them, and so that closing one is a deliberate change.

## Comparing two builds

Reading this document tells you what *may* differ. `frost journal export`
writes down what actually did: every key component of every action this
workspace has recorded — argv, environment, input digests, toolchain
fingerprint, profile, platform and the action-key schema version — in a stable
order, so two exports of one build are byte-identical.

`frost journal diff A B` then reports the first field that differs, per action,
rather than every field that does. A changed compiler makes every input digest
differ too, so a build-wide difference is reported once and alone: four
thousand consequences hide the one cause. Within an action the order is
argv, env, pass_env, inputs, outputs — decisions before their effects.

That is the tool for "CI rebuilt what my machine had cached". `frost build
--explain` answers the narrower question of why one action reran here.

The export carries a format version, and a mismatch refuses rather than
comparing fields that may since have changed meaning.
