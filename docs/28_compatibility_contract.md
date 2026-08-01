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
| `--json` output of `doctor`, `info`, `query`, `cache stats`, `lint` | Fields are added, never removed or retyped. A consumer that reads a field by name keeps working | the E2E tests that parse each one |
| `frost lint` rule identifiers | A rule keeps its name and keeps meaning the same thing, so `--allow` in a checked-in CI job keeps silencing what it was written to silence. A retired rule stops being reported and stays accepted by `--allow` | `a_misspelled_lint_rule_is_refused_rather_than_silently_allowing_nothing`, and `lint::RULES` |
| `frost info` keys | A key keeps naming the same thing. New keys are added | `info_answers_path_questions_without_a_graph` |
| `.frost-version` format | One `X.Y.Z` line, optional `#` comments, whitespace insignificant. A file `frostw` reads today keeps being read the same way, by the wrapper and by frost itself | `wrapper::tests::a_version_is_read_the_way_the_wrapper_scripts_read_it`, `this_repository_checks_in_the_wrapper_frost_writes` |
| Release asset names and `SHA256SUMS` | `frostbuild-v<version>-<triple>.{tar.gz,zip}` beside a `SHA256SUMS` listing them, under `releases/download/v<version>/`. A checked-in `frostw` from an older release keeps resolving newer ones | `frostw_fetches_verifies_and_runs_the_version_the_workspace_declares` |
| Artifact layout under the configured output tree | `.frost/out/<config>`, `.frost/bin/<config>` and the `${config}` rule stay as documented, and `frost info` answers them so callers need not encode the rule | `info_answers_path_questions_without_a_graph` |

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

### `frost lint --json`

One JSON object per line, nothing else on stdout, so `jq` and a line-oriented
reader both work without a mode. Exit `1` when there is at least one finding,
`0` when there are none, `2` when the workspace could not be read.

| Field | Type | Meaning |
|---|---|---|
| `rule` | string | The rule's stable identifier, from the table below. What `--allow` takes |
| `target` | string or `null` | The target the finding is about, spelled as the manifest spells it. `null` for a finding about the workspace rather than one target |
| `detail` | string | What was found, naming the specific thing that triggered it. Wording is not contract |
| `why` | string | What it costs. The same sentence for every finding of a rule, because the reason belongs to the rule. Wording is not contract |

The rules, each of which has a legitimate exception — that is why they are
lints and not parse errors, and why `--allow` exists:

| Rule | Reports |
|---|---|
| `unreachable-target` | A target that is not a default target and that nothing depends on. Tests are excluded: `frost test` selects them directly, so nothing depending on one means nothing |
| `redundant-pass-env` | A `pass_env` naming a variable frost passes to every action anyway. It does not make the variable available — it already is — it makes its *value* action-key material, so two machines whose values differ stop sharing cache entries |
| `absolute-path` | An absolute path inside `args`, `steps` or `cmd`. Declared paths are already a parse error; arguments are opaque to frost, which is what lets one hide there |
| `host-shell-syntax` | `&&`, `\|\|`, a pipe, a redirect, `;` or command substitution in a `cmd` run through the host shell, which `/bin/sh` and `cmd.exe` read differently. A `command` target with direct argv has no shell to disagree with |
| `missing-include-dir` | An `includes` entry that is not a directory and that no target generates. It still goes on the compiler's search path, where it finds nothing |

Three checks the manifest spec asks for are absent here because they are
already refusals rather than findings, which is strictly stronger: a glob
matching no files and a manifest naming an undeclared profile or platform are
parse-time errors, and two targets declaring one output is a graph error.

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
