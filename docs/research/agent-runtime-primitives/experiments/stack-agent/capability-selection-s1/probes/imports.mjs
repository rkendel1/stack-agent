// TS/JS import edges: Synapse modules vs the TypeScript compiler's import scanner (ts.preProcessFile).
import { call } from "./syn.mjs";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
const ts = createRequire("/home/user/appboundry/")("typescript");
const root = "/home/user/appboundry/";
const files = execFileSync("git", ["-C", root, "ls-files", "packages/*/src/*.ts", "packages/*/src/**/*.ts"], { encoding: "utf8" }).trim().split("\n").filter(f => !f.endsWith(".d.ts"));
let truth = 0, igbHit = 0, igbLines = 0, scpHit = 0, scpTotal = 0, tooBig = 0, scpFiles = 0;
const missedKinds = {};
for (const f of files) {
  const text = readFileSync(root + f, "utf8");
  const ref = new Set(ts.preProcessFile(text, true, true).importedFiles.map(i => i.fileName));
  truth += ref.size;
  // import-graph-builder: edges are raw lines; count a reference specifier as found when some edge line quotes it.
  const igb = await call("skill-14-import-graph-builder", { files: [{ path: f, content: text }] });
  const lines = (igb.result?.edges ?? igb.result?.imports ?? []).map(e => JSON.stringify(e));
  igbLines += lines.length;
  for (const s of ref) {
    if (lines.some(l => l.includes(`'${s}'`) || l.includes(`\\"${s}\\"`))) igbHit++;
    else { const line = text.split("\n").find(l => l.includes(`"${s}"`) || l.includes(`'${s}'`))?.trim() ?? ""; const k = line.startsWith("export") ? "export…from" : line.startsWith("import") ? "import (other line of multi-line)" : /import\(/.test(line) ? "dynamic import()" : line.startsWith("}") ? "multi-line import closing line" : "other"; missedKinds[k] = (missedKinds[k] ?? 0) + 1; }
  }
  if (text.length > 16384) { tooBig++; continue; }
  scpFiles++;
  const scp = await call("skill-02-source-code-parser", { source: text, language: "javascript" });
  const got = new Set(scp.result?.imports ?? []);
  scpTotal += got.size;
  for (const s of ref) if (got.has(s)) scpHit++;
}
const igbSample = await call("skill-14-import-graph-builder", { files: [{ path: "a.ts", content: 'import { x } from "./b.js";\nimport {\n  y,\n} from "./c.js";\nexport { z } from "./d.js";\nconst m = await import("./e.js");' }] });
console.log(JSON.stringify(igbSample.result).slice(0, 600));
console.log({ files: files.length, referenceSpecifiers: truth, importGraphBuilder: { found: igbHit, recall: (igbHit / truth).toFixed(3), edgeLines: igbLines, missedBy: missedKinds }, sourceCodeParser: { filesUnder16k: scpFiles, filesTooLarge: tooBig, refInThoseFiles: "see below", found: scpHit, returned: scpTotal } });
