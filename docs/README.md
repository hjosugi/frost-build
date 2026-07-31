# Documentation index

Two documents are normative. [DESIGN.md](../DESIGN.md) defines the architecture
and the numbered sections the issues cite; [06_manifest_spec.md](06_manifest_spec.md)
defines `frost.toml`. Everything else records how a decision was reached, what a
measurement showed, or where a limit currently sits — useful as evidence, not as
a contract.

Numeric prefixes are historical and three numbers are used twice (`06`, `09`,
`17`). They are not renamed because issues, pull requests and source comments
cite the existing file names.

## Normative

| Document | Contents |
|---|---|
| [DESIGN.md](../DESIGN.md) | architecture, action model, non-goals and what is deferred to v2 |
| [06_manifest_spec.md](06_manifest_spec.md) | `frost.toml`: targets, profiles, platforms, command adapter, owned output directories, dependency report formats |

## Architecture and strategy

| Document | Contents |
|---|---|
| [00_world_fastest_build_tools.md](00_world_fastest_build_tools.md) | survey of what the fastest build systems actually do |
| [01_architecture_nix_bazel_micro_partition.md](01_architecture_nix_bazel_micro_partition.md) | hermetic-store and micro-partition foundations |
| [02_two_x_strategy.md](02_two_x_strategy.md) | where the intended speedup comes from |
| [14_bazel_gap_analysis.md](14_bazel_gap_analysis.md) | what Bazel has that Frost does not, and which gaps matter |
| [15_research_cache_layers.md](15_research_cache_layers.md) | separation of cache layers 1/2/3 |
| [16_action_key_audit.md](16_action_key_audit.md) | what the action key covers and why each field is in it |
| [22_developer_loop.md](22_developer_loop.md) | the edit/build/run loop Frost is optimizing |
| [28_compatibility_contract.md](28_compatibility_contract.md) | what a release promises not to break, and what it explicitly does not |
| [29_sample_workspaces.md](29_sample_workspaces.md) | the five checked-in workspaces, and when to own the compiler versus wrap an ecosystem build |

## Research and design studies

| Document | Contents |
|---|---|
| [03_papers_and_references.md](03_papers_and_references.md) | primary sources |
| [07_remote_cache_study.md](07_remote_cache_study.md) | REAPI cache decision (implementation tracked in the issue tracker) |
| [08_predictive_selection.md](08_predictive_selection.md) | predictive action selection |
| [09_learned_scheduling.md](09_learned_scheduling.md) | learned duration estimates and scheduling |
| [10_language_adapters.md](10_language_adapters.md) | Rust / TypeScript / Go adapter design |
| [11_remote_execution_study.md](11_remote_execution_study.md) | external BuildGrid/BuildBox REAPI certificate and v2 adapter gaps |
| [23_bazel_migration.md](23_bazel_migration.md) | conservative Bazel import path |
| [25_npm_workspace_import.md](25_npm_workspace_import.md) | npm workspace gates and explicit Vite build discovery |
| [26_deltacdc_remote_calibration.md](26_deltacdc_remote_calibration.md) | fresh corpus, RPC and CPU/bandwidth decision for remote DeltaCDC |
| [27_npm_production_adoption.md](27_npm_production_adoption.md) | real npm/Vite production adoption certificate and boundary policy |

## Comparisons and measurement

| Document | Contents |
|---|---|
| [05_benchmark_methodology.md](05_benchmark_methodology.md) | how benchmarks are run and reported |
| [17_java_gradle_maven_comparison.md](17_java_gradle_maven_comparison.md) | Java against Gradle and Maven |
| [18_polyglot_win_matrix.md](18_polyglot_win_matrix.md) | per-language definition of "win" |
| [19_rust_cargo_comparison.md](19_rust_cargo_comparison.md) | Rust against Cargo |
| [20_go_build_comparison.md](20_go_build_comparison.md) | Go against `go build` |
| [21_typescript_tsc_comparison.md](21_typescript_tsc_comparison.md) | TypeScript against `tsc` |
| [24_python_wheel_comparison.md](24_python_wheel_comparison.md) | Python wheel packing |

## Platform, correctness and process

| Document | Contents |
|---|---|
| [09_platform_support.md](09_platform_support.md) | host and target support, and which tests run on which host |
| [12_fuzzing.md](12_fuzzing.md) | fuzz and property-testing surface |
| [13_issue_implementation_matrix.md](13_issue_implementation_matrix.md) | which issue each implementation and evidence gate belongs to |

## Historical and superseded

| Document | Contents |
|---|---|
| [04_zig_implementation_plan.md](04_zig_implementation_plan.md) | the abandoned Zig prototype plan; Rust is authoritative |
| [06_ninja_importer.md](06_ninja_importer.md) | Ninja subset importer notes |
| [17_session_log_2026-07.md](17_session_log_2026-07.md) | dated snapshot of the July 2026 investigation |
