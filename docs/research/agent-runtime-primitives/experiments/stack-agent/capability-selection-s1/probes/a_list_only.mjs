// Scenario A: only what `work` observes in a non-Cargo project: project.list (paths + sizes).
import { call } from "./graph_lib.mjs";
import { execFileSync } from "node:child_process";
import { statSync } from "node:fs";
const root = "/home/user/stack-agent";
const paths = execFileSync("git", ["-C", root, "ls-files", "crates/chip-experiments"], { encoding: "utf8" }).trim().split("\n");
const files = paths.map(path => ({ path, bytes: statSync(`${root}/${path}`).size }));
const { graph } = await call("repository-graph-builder", { files });
const rels = {}; for (const e of graph.edges) rels[e.relationship] = (rels[e.relationship] ?? 0) + 1;
console.log(`files=${files.length} nodes=${graph.nodes.length} edges=${graph.edges.length} relationships=${JSON.stringify(rels)}`);
const start = "file:crates/chip-experiments/src/plugin.rs";
const t = await call("repository-impact-traverser", { graph, startNodes: [start], maxDepth: 3 });
console.log(`traverse from ${start}: direct=${t.direct.map(h => h.nodeId)} transitive=${t.transitive.length} (all: ${[...t.direct, ...t.transitive].filter(h => h.nodeId.startsWith("file:")).map(h => h.nodeId.slice(5)).join(", ")})`);
console.log(`AppBoundry contract cap 200 nodes/200 edges -> this graph ${graph.nodes.length > 200 || graph.edges.length > 200 ? "EXCEEDS" : "fits"}`);
