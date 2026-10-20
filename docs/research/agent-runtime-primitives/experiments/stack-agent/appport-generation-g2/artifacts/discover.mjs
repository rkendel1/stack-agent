// Runs the patched CLI's own discovery over a tree and prints what it found (read-only).
import { execFileSync } from "node:child_process";
const [cliDist, root] = process.argv.slice(2);
const { discoverCapabilityCandidates } = await import(`${cliDist}/commands.js`);
const paths = execFileSync("find", [".", "-type", "f"], { cwd: root, encoding: "utf8" }).split("\n").filter(Boolean).map(p => p.replace(/^\.\//, ""));
const found = await discoverCapabilityCandidates(root, paths);
const bySurface = {};
for (const c of found) (bySurface[c.surface] ??= []).push(c);
console.log(JSON.stringify({ files: paths.length, total: found.length, surfaces: Object.fromEntries(Object.entries(bySurface).map(([k, v]) => [k, v.length])) }));
for (const c of found.filter(c => c.surface === "http").slice(0, 400)) console.log(`${c.method ?? "?"} ${c.target}  name=${c.name} source=${c.source} rank=${c.rank} input=${JSON.stringify(c.inputSchema ?? null).slice(0, 80)}`);
for (const c of found.filter(c => c.surface !== "http").slice(0, 20)) console.log(`[${c.surface}] ${c.target} name=${c.name} source=${c.source}`);
