import { call } from "./syn.mjs";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
const ts = createRequire("/home/user/appboundry/")("typescript");
const root = "/home/user/appboundry/";
const files = execFileSync("git", ["-C", root, "ls-files", "packages/*/src/*.ts", "packages/*/src/**/*.ts"], { encoding: "utf8" }).trim().split("\n").filter(f => !f.endsWith(".d.ts"));
let ref = 0, hit = 0, ret = 0, wrong = 0, bigRef = 0;
for (const f of files) {
  const text = readFileSync(root + f, "utf8");
  const r = new Set(ts.preProcessFile(text, true, true).importedFiles.map(i => i.fileName));
  if (text.length > 16384) { bigRef += r.size; continue; }
  ref += r.size;
  const got = new Set((await call("skill-02-source-code-parser", { source: text, language: "javascript" })).result?.imports ?? []);
  ret += got.size; for (const s of r) if (got.has(s)) hit++; for (const g of got) if (!r.has(g)) wrong++;
}
console.log({ refUnder16k: ref, found: hit, recallUnder16k: (hit / ref).toFixed(3), returned: ret, notInReference: wrong, refInRejectedLargeFiles: bigRef, overallRecall: (hit / (ref + bigRef)).toFixed(3) });
