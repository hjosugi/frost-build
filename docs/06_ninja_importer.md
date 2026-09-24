# Ninja Importer

`frost import-ninja build.ninja --output frost.toml` converts a small, explicit
Ninja subset into `frost.toml`. The Python reference command remains available
for comparison.
The goal is migration smoke testing, not complete Ninja compatibility. The
step-by-step procedure is [the user guide's Ninja migration](guide/migrate/ninja.md).

Every edge becomes a `genrule` that runs the edge's command through the host
shell, in the directory holding `build.ninja` — which is why the manifest must
be written next to it, and why a path containing `..` or an absolute path is
refused.

Supported:

- top-level `var = value` assignments, evaluated when defined, and `$var` /
  `${var}` references to them (an unknown variable is empty, as in Ninja)
- `$$`, `$ `, `$:` escapes and `$`-newline continuations
- `rule NAME` blocks with `command = ...` (`description` is accepted and ignored)
- `build OUT [| IMPLICIT_OUT]: RULE IN [| IMPLICIT] [|| ORDER_ONLY]`; `$in` and
  `$out` become the edge's explicit paths, shell-quoted as Ninja quotes them
- inputs produced by another edge become `deps`, other inputs become declared
  `inputs`; implicit inputs are inputs and order-only inputs are treated as full
  inputs, which can only rebuild more than Ninja, never less
- `build NAME: phony PATHS` as an alias, resolved wherever it is referenced
- `default PATHS`, resolved through aliases, as `default_targets`

Refused, with the file and line, so no silently incorrect graph is written:

- `depfile`, `deps` and `msvc_deps_prefix` on a rule: a genrule cannot ingest
  the reported headers, so a header edit would not rebuild
- per-edge variable bindings, which would change the command the rule names
- `include`, `subninja`, `pool` declarations and any other rule attribute
- validations (`|@`) and `$in_newline`
- a command containing `${`, which a Frost genrule would read as its own
  substitution
- two edges producing one path, an alias that is also an output, and a
  `default` naming a file no edge produces
- an existing manifest at `--output`: like every other importer, it never
  overwrites one
