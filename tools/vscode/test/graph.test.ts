import * as assert from 'node:assert/strict';
import { test } from 'node:test';

import { parseDotGraph, rootTargets } from '../src/frost/graph';

// Captured verbatim from `frost graph --dot` on sample_multi. Keeping the real
// output as the fixture is the point: a change to the emitted shape shows up
// here rather than as an empty target list in the editor.
const SAMPLE_MULTI = `digraph frost {
  rankdir=LR;
  "//apps/cli:cli" [shape=box];
  "//apps/cli:cli" -> "//text:text";
  "//apps/cli:cli" -> "//render:render";
  "//core:core" [shape=ellipse];
  "//core:core" -> "gen_version";
  "//core:core_test" [shape=box3d];
  "//core:core_test" -> "//core:core";
  "//render:render" [shape=ellipse];
  "//render:render" -> "//core:core";
  "//text:text" [shape=ellipse];
  "//text:text" -> "//core:core";
  "gen_version" [shape=diamond];
}
`;

test('every declared target is found, including one only named by an edge', () => {
  const graph = parseDotGraph(SAMPLE_MULTI);
  assert.deepEqual(
    [...graph.targets].sort(),
    [
      '//apps/cli:cli',
      '//core:core',
      '//core:core_test',
      '//render:render',
      '//text:text',
      'gen_version',
    ],
    'the universe is every node in the graph',
  );
});

test('edges are read in order and both endpoints registered', () => {
  const graph = parseDotGraph(SAMPLE_MULTI);
  assert.deepEqual(graph.edges.slice(0, 2), [
    { from: '//apps/cli:cli', to: '//text:text' },
    { from: '//apps/cli:cli', to: '//render:render' },
  ]);
  assert.equal(graph.edges.length, 6);
});

test('shapes are ignored', () => {
  // Regression guard for the decision recorded in graph.ts: shapes encode the
  // target kind but are a rendering choice, not a contract. Nothing in the
  // parsed result may carry one, or a cosmetic change upstream becomes a bug.
  const graph = parseDotGraph(SAMPLE_MULTI);
  assert.ok(!JSON.stringify(graph).includes('box3d'));
  assert.ok(!JSON.stringify(graph).includes('shape'));
});

test('roots are the targets nothing depends on', () => {
  const graph = parseDotGraph(SAMPLE_MULTI);
  // cli is a root; core_test is one too, because a test target is depended on
  // by nothing. Missing that would leave every test out of the target list.
  assert.deepEqual(rootTargets(graph), ['//apps/cli:cli', '//core:core_test']);
});

test('deps of the roots cover every target', () => {
  // The property the whole approach rests on: union of deps(root) == universe.
  const graph = parseDotGraph(SAMPLE_MULTI);
  const roots = rootTargets(graph);
  const reachable = new Set<string>();
  const walk = (label: string): void => {
    if (reachable.has(label)) {
      return;
    }
    reachable.add(label);
    for (const edge of graph.edges) {
      if (edge.from === label) {
        walk(edge.to);
      }
    }
  };
  roots.forEach(walk);
  assert.deepEqual([...reachable].sort(), [...graph.targets].sort());
});

test('a graph with no roots falls back to every target', () => {
  // frost rejects cycles, so this input is not a frost graph — but returning
  // nothing would silently empty the editor's target list, and querying
  // everything is merely slower.
  const cyclic = 'digraph frost {\n  "a" -> "b";\n  "b" -> "a";\n}\n';
  assert.deepEqual(rootTargets(parseDotGraph(cyclic)), ['a', 'b']);
});

test('CRLF input parses identically', () => {
  const crlf = SAMPLE_MULTI.replace(/\n/g, '\r\n');
  assert.deepEqual(parseDotGraph(crlf), parseDotGraph(SAMPLE_MULTI));
});

test('unrecognized lines cost nothing', () => {
  const noisy = SAMPLE_MULTI.replace(
    '  rankdir=LR;',
    '  rankdir=LR;\n  // a comment a future version might add\n  label="frost";',
  );
  assert.deepEqual(parseDotGraph(noisy), parseDotGraph(SAMPLE_MULTI));
});

test('a quoted label containing an escape survives', () => {
  // Not emitted today, but the parser must not silently truncate a label if it
  // ever is: a wrong label would build the wrong target.
  const graph = parseDotGraph('digraph frost {\n  "a\\"b" [shape=box];\n}\n');
  assert.deepEqual(graph.targets, ['a"b']);
});

test('an empty graph yields nothing rather than throwing', () => {
  const graph = parseDotGraph('digraph frost {\n  rankdir=LR;\n}\n');
  assert.deepEqual(graph.targets, []);
  assert.deepEqual(rootTargets(graph), []);
});
