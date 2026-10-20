import { call } from "./syn.mjs";
import { readFileSync } from "node:fs";
const ref = JSON.parse(readFileSync("toml_ref.json", "utf8"));
const canon = v => JSON.stringify(v, (k, x) => x && typeof x === "object" && !Array.isArray(x) ? Object.fromEntries(Object.entries(x).sort()) : x);
const tally = {};
for (const f of ref) {
  const text = readFileSync(f.abs, "utf8");
  const r = await call("skill-02-toml-parser", { source: text });
  const k = !r.ok ? "error" : r.result.status !== "parsed" && r.result.status !== "computed" ? r.result.status : canon(r.result.value ?? r.result.data ?? r.result.document) === canon(f.ref) ? "equal" : "DIFFERENT (silent misread)";
  tally[k] = (tally[k] ?? 0) + 1; if (k.startsWith("DIFF")) console.log("  misread:", f.abs.replace("/home/user/",""));
}
console.log(ref.length, tally);
const s = await call("skill-02-toml-parser", { source: 'a = 1\n' }); console.log(JSON.stringify(s).slice(0, 200));
