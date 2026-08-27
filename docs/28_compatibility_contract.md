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
| `--json` output of `doctor`, `info`, `query`, `cache stats`, `daemon status`, `lint` | Fields are added, never removed or retyped. A consumer that reads a field by name keeps working | the E2E tests that parse each one |
| `frost lint` rule identifiers | A rule keeps its name and keeps meaning the same thing, so a `lint_allow` written into a manifest keeps silencing what it was written to silence. A retired rule stops being reported and stays accepted where it is named. The rules themselves are in [06_manifest_spec.md](06_manifest_spec.md#frost-lint) | `this_repository_and_its_samples_pass_their_own_lint` |
| `frost info` keys | A key keeps naming the same thing. New keys are added | `info_answers_path_questions_without_a_graph` |
| `.frost-version` format | One `X.Y.Z` line, optional `#` comments, whitespace insignificant. A file `frostw` reads today keeps being read the same way, by the wrapper and by frost itself | `wrapper::tests::a_version_is_read_the_way_the_wrapper_scripts_read_it`, `this_repository_checks_in_the_wrapper_frost_writes` |
| Release assets and `SHA256SUMS` | `frostbuild-v<version>-<triple>.{tar.gz,zip}` beside a `SHA256SUMS` listing the archives, under `releases/download/v<version>/`. Archives retain `frost`, `frostd`, `share/man/man1` and `share/completions`; releases also retain `install.sh`, `frostbuild.rb` and `frostbuild.json`. A checked-in `frostw`, installer or self-update client from an older release keeps resolving newer ones | wrapper/self-update E2E, `tests/test_distribution.py`, and the release asset smoke job |
| `--build-event-json` events | Every line carries `schema`. Fields are added, never removed or retyped; `event` and `result` names keep their meaning. A bump means a field changed meaning or left | `the_build_event_stream_is_ndjson_a_ci_job_can_read` |
| Artifact layout under the configured output tree | `.frost/out/<config>`, `.frost/bin/<config>` and the `${config}` rule stay as documented, and `frost info` answers them so callers need not encode the rule | `info_answers_path_questions_without_a_graph` |

The event-stream row has a reader inside this repository, which is how the
additive half of that promise is checked rather than only stated:
`scripts/frost_junit.py` ignores events and fields it has never heard of, and
refuses a `schema` that is not the one it knows — `tests/test_frost_junit.py`
drives both, plus the one case an additive change cannot cover, a `result` name
the reader is too old to understand.

### Exit codes

| Code | Class | Meaning | What falls in it |
|---|---|---|---|
| `0` | result | the requested work completed | a build, test or query that finished; a fully cached build; `doctor` finding everything it requires |
| `1` | result | the work ran and did not succeed | a compile or link failed; a test failed; a determinism check found a difference; `doctor` found a missing required tool; a query whose answer is legitimately empty (`somepath` with no path) |
| `2` | refusal | frost could not run the work as asked | an unparsable command line; an unknown target, profile or platform; a manifest that does not parse or does not validate; a missing or unreadable workspace; a configured tool that is not executable; an internal error |

The distinction that matters to a script is `1` versus `2`: `1` is an answer
about your code, `2` is an answer about your invocation or environment. A
mistyped target name, an unreadable `frost.toml` and a tool frost cannot find
are all `2`; a compile or test that ran and failed is `1`.
`exit_codes_separate_your_code_from_your_invocation` drives one case of each
through the real binary, because a document that says so and a binary that does
so are different claims; `exit_codes_separate_a_bad_invocation_from_a_bad_build`
covers the rest of the refusal rows — an unknown profile, an unknown platform, a
query for a target that does not exist — on every host rather than only on unix.

### What a refusal says

A `2` is frost declining to act, so it owes the reader the way forward. Each of
these is enforced by a test rather than by intent:

- an unknown target lists up to three targets it might have been, spelled as
  labels, and points at `frost query deps //...` when the workspace is too
  large to enumerate;
- a manifest error is `path:line:column: problem`, workspace-relative, with the
  offending line, a caret over the span the parser recorded, and — when a valid
  alternative is close enough to what was written — `did you mean`;
- a configured tool that is not executable names the manifest key that declared
  it, where frost looked, which targets needed it, and `frost doctor`.

The wording of any of it is [not contract](#not-contract). Which class an exit
code falls in is.

## What the VS Code extension depends on

`tools/vscode/` is a consumer inside this repository, so the surfaces it parses
are listed here rather than discovered by breaking it. Each is already covered
by a row above; this says which ones an editor would notice first.

| Surface | Used for |
|---|---|
| `frost info --json` | workspace root, `config`, and the output/bin directories a launch configuration needs |
| `frost daemon status --json` | the status-bar daemon state and protocol compatibility indicator |
| `frost query targets --output label-kind` | the sidebar tree and every target picker |
| `frost query owners <path> --output label-kind` | "build the target that owns this file" |
| `frost query <fn> --json` | the `targets` array, for pickers that do not need kinds |
| `<kind> target <label>`, the `--output label-kind` line shape | parsed with an anchored three-field pattern |
| `frost build/test --no-tui` diagnostics, including the `(//pkg:target)` suffix | Problems entries attributed to the target frost blamed |
| Exit code 1 from a query that matched nothing | an empty answer, distinguished from a failure |

The `--output dot` shapes are **not** in this list. They encode target kind as
a node shape, which is a rendering choice; the extension read them once and the
workaround was deleted when `frost query targets` replaced it.

## Not contract

These change whenever there is a reason, in any release, without a deprecation
period. Depending on them is a choice to track the implementation.

| Surface | Why it is not contract |
|---|---|
| Everything under `.frost/` | The graph store, journal, hash cache, CAS layout and no-op certificate are an implementation of incrementality. They are versioned so that a change is detected, not so that a change is avoided |
| Action-key construction | The key covers whatever correctness requires. Adding a field is a cache miss, which is the safe direction. `frost info action_key_schema` reports the current layout so tooling can observe a bump instead of inferring one |
| Internal crate APIs (`frostbuild-core`, `-exec`, `-store`, `-daemon`) | The published artifact is the `frost` binary. The crates are how it is built |
| Progress, log and diagnostic text | Human-facing output is improved continuously. Machine consumers use `--json`, the Chrome trace or the build event stream |
| `frost lsp` beyond the protocol itself | Which LSP capabilities are advertised, and what a completion list or hover contains, follow what is useful to an editor. The protocol framing and the meaning of a diagnostic's message are not this row: the message is the loader's, covered where the loader is |
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
