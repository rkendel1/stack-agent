import { call } from "./syn.mjs";
import { readFileSync } from "node:fs";
const ref = JSON.parse(readFileSync("md_ref.json", "utf8"));
let refused = 0, n = 0, rl = 0, ml = 0, linkHit = 0, linkExtra = 0, rh = 0, hHit = 0, exactFiles = 0, readmeRefused = 0, readmes = 0;
const decode = s => { try { return decodeURI(s); } catch { return s; } };
for (const f of ref) {
  const isReadme = /(^|\/)README\.md$/i.test(f.path); if (isReadme) readmes++;
  if (f.chars > 16384) { refused++; if (isReadme) readmeRefused++; continue; }
  const text = readFileSync(f.abs, "utf8");
  const r = (await call("skill-02-markdown-parser", { source: text })).result;
  if (!r || r.status !== "computed" && !r.links) { refused++; continue; }
  n++;
  const got = (r.links ?? []).map(l => typeof l === "string" ? l : l.href ?? l.url);
  const want = f.links.map(decode);
  rl += want.length; ml += got.length;
  const pool = [...got]; let hit = 0;
  for (const w of want) { const i = pool.indexOf(w); if (i >= 0) { pool.splice(i, 1); hit++; } }
  linkHit += hit; linkExtra += pool.length;
  const gh = (r.blocks ?? []).filter(b => b.type === "heading").map(b => b.text);
  rh += f.headings.length; hHit += f.headings.filter(h => gh.includes(h)).length;
  if (hit === want.length && pool.length === 0) exactFiles++;
}
console.log({ markdownFiles: ref.length, refusedOver16k: refused, readmes, readmesRefused: readmeRefused, analyzed: n, filesWithExactLinkSet: exactFiles, refLinks: rl, foundLinks: linkHit, linkRecall: (linkHit / rl).toFixed(3), extraLinks: linkExtra, linkPrecision: (linkHit / (linkHit + linkExtra)).toFixed(3), refHeadings: rh, headingsExact: hHit, headingRecall: (hHit / rh).toFixed(3) });
