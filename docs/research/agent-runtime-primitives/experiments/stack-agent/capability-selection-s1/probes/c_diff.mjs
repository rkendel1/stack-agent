import { call } from "./graph_lib.mjs";
const base = [{ path: "packages/workspace/README.md", bytes: 1200 }, { path: "packages/workspace/src/index.ts", bytes: 800 }, { path: "package.json", bytes: 300 }];
const before = (await call("repository-graph-builder", { files: base })).graph;
const cases = {
  "README content edited (size changed)": base.map(f => f.path.endsWith("README.md") ? { ...f, bytes: 1650 } : f),
  "README edited, same size": base,
  "unintended extra file written": [...base, { path: "packages/workspace/NOTES.md", bytes: 90 }],
  "README deleted": base.filter(f => !f.path.endsWith("README.md")),
};
for (const [name, files] of Object.entries(cases)) {
  const after = (await call("repository-graph-builder", { files })).graph;
  const g = await call("graph-impact-differ", { before, after });
  const r = await call("repository-graph-differ", { before, after });
  const brief = o => Object.fromEntries(Object.entries(o).filter(([k, v]) => Array.isArray(v) && v.length).map(([k, v]) => [k, v.map(x => typeof x === "string" ? x : x.id)]));
  console.log(`\n${name}\n  graph-impact-differ:     ${JSON.stringify(brief(g))}\n  repository-graph-differ: ${JSON.stringify(brief(r))}`);
}
