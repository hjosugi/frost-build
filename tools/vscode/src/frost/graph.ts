// Reading the target universe out of `frost graph --dot`.
//
// There is no "list every target" query today: `query deps` and `query rdeps`
// both need a target to start from, and `pick` needs fzf. So the universe is
// derived — the dot graph names every target and every edge, which is enough to
// find the roots, and `query deps <root> --output label-kind` (a contract
// surface) then supplies the kinds.
//
// Only the node names and edges are read here, never the shapes. The shapes
// encode target kind, but they are a rendering choice rather than a documented
// promise, and inferring `cc_test` from `box3d` would make the extension break
// on a cosmetic change. Asking `query` for the kind costs one more invocation
// and is answerable from the contract.
//
// The right fix is a CLI primitive that lists the universe with kinds in one
// call; until then this keeps the extension on documented ground.

/** Target labels and dependency edges parsed out of `frost graph --dot`. */
export interface DotGraph {
  /** Every declared node, in first-seen order. */
  targets: string[];
  /** `from -> to` pairs, in first-seen order. */
  edges: { from: string; to: string }[];
}

const NODE = /^\s*"((?:[^"\\]|\\.)*)"\s*\[/;
const EDGE = /^\s*"((?:[^"\\]|\\.)*)"\s*->\s*"((?:[^"\\]|\\.)*)"\s*;?\s*$/;

function unquote(raw: string): string {
  return raw.replace(/\\(.)/g, '$1');
}

/**
 * Parse the node and edge lines of a dot graph.
 *
 * Anything else — the `digraph` header, `rankdir`, the closing brace, comments
 * a future version might add — is skipped rather than treated as an error. This
 * is a derived reading of a presentation format, so tolerance is the correct
 * posture: an unrecognized line should cost nothing.
 */
export function parseDotGraph(text: string): DotGraph {
  const targets: string[] = [];
  const seen = new Set<string>();
  const edges: { from: string; to: string }[] = [];
  const push = (label: string): void => {
    if (!seen.has(label)) {
      seen.add(label);
      targets.push(label);
    }
  };
  for (const raw of text.split('\n')) {
    const line = raw.replace(/\r$/, '');
    const edge = EDGE.exec(line);
    if (edge) {
      const from = unquote(edge[1]);
      const to = unquote(edge[2]);
      push(from);
      push(to);
      edges.push({ from, to });
      continue;
    }
    const node = NODE.exec(line);
    if (node) {
      push(unquote(node[1]));
    }
  }
  return { targets, edges };
}

/**
 * Targets nothing else depends on.
 *
 * Every target is reachable from some root — a target with no dependents is one
 * itself — so the union of `deps(root)` over these covers the universe. Sorted
 * so the resulting query order, and therefore the cache, is deterministic.
 */
export function rootTargets(graph: DotGraph): string[] {
  const depended = new Set(graph.edges.map((edge) => edge.to));
  const roots = graph.targets.filter((target) => !depended.has(target));
  // A cycle would leave no roots at all. frost rejects dependency cycles when
  // it builds the graph, so reaching this means the input was not a frost
  // graph; querying every target is a correct, if slower, answer.
  return (roots.length > 0 ? roots : graph.targets).slice().sort();
}
