// Scenario B: feed Stack-Agent's own source-derived graph (chip-graph snapshot) into the Synapse
// builder as `relationships`, then compare repository-impact-traverser with the truth set:
// files that actually reference the start file's items (grep), and chip-graph's own Imports edges.
import { call } from "./graph_lib.mjs";
import { readFileSync } from "node:fs";
const snap = JSON.parse(readFileSync(process.argv[2], "utf8"));
const START = process.argv[3];
const nodes = snap.nodes, edges = snap.edges;
console.log("snapshot nodes", nodes.length, "edges", edges.length, "edge kinds", JSON.stringify(edges.reduce((m, e) => (m[e.kind] = (m[e.kind] ?? 0) + 1, m), {})));
console.log("sample node", JSON.stringify(nodes.find(n => n.kind?.toLowerCase?.() === "file") ?? nodes[0]), "sample edge", JSON.stringify(edges[0]));
// Restrict to the closed neighbourhood that fits the AppBoundry contract (200 nodes/200 edges).
const ascii = s => /^[\x20-\x7e]+$/.test(s);
const touching = edges.filter(e => ascii(e.from) && ascii(e.to));
const relationships = touching.map(e => ({ sourceNodeId: e.from, targetNodeId: e.to, relationship: String(e.kind).toLowerCase(), source: "chip-graph", certainty: "verified", confidence: 1 }));
const { graph } = await call("repository-graph-builder", { relationships });
console.log("synapse graph from chip-graph edges: nodes", graph.nodes.length, "edges", graph.edges.length, graph.edges.length > 200 ? "(EXCEEDS AppBoundry 200-edge cap)" : "(fits)");
for (const direction of ["upstream", "downstream", "both"]) {
  const t = await call("repository-impact-traverser", { graph, startNodes: [START], direction, maxDepth: 3 });
  const hits = [...t.direct, ...t.transitive].map(h => h.nodeId);
  console.log(`\n${direction}: ${hits.length} hits, truncated=${t.truncated}; files: ${hits.filter(h => /\.rs$/.test(h)).slice(0, 25).join(" ")}`);
}
