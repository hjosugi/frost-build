# Compatibility contract

What a FrostBuild release promises not to break, what it explicitly does not
promise, and what happens when something has to change anyway.

1.0 is not a statement about speed or feature count. It is a statement about
which surfaces callers may depend on. This document draws that line, and every
line it draws is enforced by a test rather than by intent — where it is not,
the entry says so.

Before 1.0, breaking changes are allowed in minor versions and are announced in
[CHANGELOG.md](../CHANGELOG.md). From 1.0, the surfaces marked **contract**
below change only in a major version, after the deprecation procedure at the
end of this document.

## Contract

| Surface | What is promised | Enforced by |
|---|---|---|
| `frost.toml` grammar and keys | A manifest that loads keeps loading. Keys are added, not repurposed; a key's meaning does not change under the same name | [06_manifest_spec.md](06_manifest_spec.md) plus the parser's own test suite |
| CLI subcommand names, long options, positionals | A command line that works keeps working. Options are added, not renamed | `cli_surface_tests::the_command_surface_matches_the_checked_in_contract`, against `crates/frostbuild-cli/tests/cli-surface.txt` |
| Exit codes | The three outcomes below stay distinguishable | `cli_surface_tests::exit_codes_keep_their_documented_meanings` |
| `--json` output of `doctor`, `info`, `query`, `cache stats` | Fields are added, never removed or retyped. A consumer that reads a field by name keeps working | the E2E tests that parse each one |
| `frost info` keys | A key keeps naming the same thing. New keys are added | `info_answers_path_questions_without_a_graph` |
| `.frost-version` format | One `X.Y.Z` line, optional `#` comments, whitespace insignificant. A file `frostw` reads today keeps being read the same way, by the wrapper and by frost itself | `wrapper::tests::a_version_is_read_the_way_the_wrapper_scripts_read_it`, `this_repository_checks_in_the_wrapper_frost_writes` |
| Release asset names and `SHA256SUMS` | `frostbuild-v<version>-<triple>.{tar.gz,zip}` beside a `SHA256SUMS` listing them, under `releases/download/v<version>/`. A checked-in `frostw` from an older release keeps resolving newer ones | `frostw_fetches_verifies_and_runs_the_version_the_workspace_declares` |
| Artifact layout under the configured output tree | `.frost/out/<config>`, `.frost/bin/<config>` and the `${config}` rule stay as documented, and `frost info` answers them so callers need not encode the rule | `info_answers_path_questions_without_a_graph` |

### Exit codes

| Code | Meaning | Examples |
|---|---|---|
| `0` | the requested work completed | build succeeded or was already up to date; `doctor` found everything |
| `1` | the work ran and did not succeed | a compile failed; a test failed; `doctor` found a missing required tool |
| `2` | frost could not run the work as asked | unusable command line, missing or invalid manifest, unreadable workspace, internal error |

The distinction that matters to a script is `1` versus `2`: `1` is an answer
about your code, `2` is an answer about your invocation or environment.

## Not contract

These change whenever there is a reason, in any release, without a deprecation
period. Depending on them is a choice to track the implementation.

| Surface | Why it is not contract |
|---|---|
| Everything under `.frost/` | The graph store, journal, hash cache, CAS layout and no-op certificate are an implementation of incrementality. They are versioned so that a change is detected, not so that a change is avoided |
| Action-key construction | The key covers whatever correctness requires. Adding a field is a cache miss, which is the safe direction. `frost info action_key_schema` reports the current layout so tooling can observe a bump instead of inferring one |
| Internal crate APIs (`frostbuild-core`, `-exec`, `-store`, `-daemon`) | The published artifact is the `frost` binary. The crates are how it is built |
| Progress, log and diagnostic text | Human-facing output is improved continuously. Machine consumers use `--json`, the Chrome trace or the build event stream |
| `--report` HTML structure | The document is a rendering for a person to read: its elements, classes, wording and layout change whenever a clearer one exists. What the numbers in it *mean* is contract, because they are the journal's durations, the scheduler's critical path and `--explain`'s reasons, each covered where it is defined. Parse the trace or `--json`, never this |
| Benchmark numbers | Measurements of a host, not promises. See [05_benchmark_methodology.md](05_benchmark_methodology.md) |
| Daemon socket path and wire protocol | The daemon is an optimization behind the same CLI; a client and server that disagree fall back to an in-process build |

## On-disk state: detect, then rebuild

Every stored format carries a version or magic marker, and every reader treats
an unrecognized one as *absent* rather than as an error or, worse, as data.
Reconstructing state from the workspace is always possible; misreading it is
not always detectable later.

| File | Marker | Behavior on mismatch |
|---|---|---|
| `.frost/graph-<config>.bin` | `FRSTGR01` + `VERSION` | recompile the graph from the manifest |
| `.frost/journal.bin` | `FRSTJR01` | decode the validated prefix; a foreign magic yields an empty journal |
| `.frost/hashcache.bin` | `FRSTHC02` | start from an empty cache and re-hash |
| `.frost/cas/manifests/*` | chunk-manifest version | ignore the manifest; restore from the whole blob or rebuild |
| no-op certificate | `FRSTNO03` | miss, and take the full check path |

The consequence is uniform: a `.frost/` written by another version costs time,
never correctness. `stale_on_disk_state_is_rebuilt_rather_than_misread`
asserts this for every format at once.

## Changing something in the contract

1. **Additive change** — a new subcommand, option, manifest key or JSON field.
   Ship it. Refresh the CLI snapshot with `UPDATE_CLI_SURFACE=1 cargo test -p
   frostbuild-cli --bin frost`. No deprecation period applies.
2. **Rename** — keep the old spelling working as a hidden alias, and warn once
   per invocation on stderr, naming the replacement. The alias lives for at
   least one minor release before removal, and both the introduction and the
   removal appear in the CHANGELOG.
3. **Removal** — announce it in a release with a warning emitted whenever the
   surface is used, and remove it no earlier than the next minor release.
   Before 1.0 the removal may land in a minor version; from 1.0 it waits for a
   major.
4. **Semantic change under an unchanged name** — not allowed. Introduce the new
   behavior under a new name and deprecate the old one, so a caller that has
   not read the CHANGELOG cannot silently get different results.

There is currently no deprecated surface. The alias-and-warn mechanism, and
the test that demonstrates it, land with the first deprecation rather than
being written against a hypothetical one.
