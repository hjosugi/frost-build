## Standing approval for FrostBuild maintenance (2026-11-28 JST)

When the working repository and the verified GitHub push destination are both
`hjosugi/frost-build`, the user grants standing approval across sessions for
requested routine issue, PR and branch maintenance. This includes local edits,
tests and commits, ordinary pushes to the task branch, opening and updating a
PR into `main`, rerunning CI, merging agent-authored or Dependabot PRs after
their checks pass, closing resolved issues, and deleting their merged task
branches. Verify the repository, refs, PR base and authorship, announce the
batch, then carry it through without asking for confirmation again. The
personal rules for external repositories, human-authored work, legal
agreements and destructive administration continue to apply.

## graphify

This project has a knowledge graph at graphify-out/ with god nodes, community structure, and cross-file relationships.

When the user types `/graphify`, use the installed graphify skill or instructions before doing anything else.

Rules:
- For codebase questions, first run `graphify query "<question>"` when graphify-out/graph.json exists. Use `graphify path "<A>" "<B>"` for relationships and `graphify explain "<concept>"` for focused concepts. These return a scoped subgraph, usually much smaller than GRAPH_REPORT.md or raw grep output.
- Dirty graphify-out/ files are expected after hooks or incremental updates; dirty graph files are not a reason to skip graphify. Only skip graphify if the task is about stale or incorrect graph output, or the user explicitly says not to use it.
- If graphify-out/wiki/index.md exists, use it for broad navigation instead of raw source browsing.
- Read graphify-out/GRAPH_REPORT.md only for broad architecture review or when query/path/explain do not surface enough context.
- After modifying code, run `graphify update .` to keep the graph current (AST-only, no API cost).
