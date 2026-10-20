import { readFile } from "node:fs/promises";
const F = "/home/user/appboundry/packages/cli/test/fixtures/synapse/";
async function call(mod, input) {
  const { instance } = await WebAssembly.instantiate(await readFile(F + mod + "/browser_agent.wasm"), {});
  const ex = instance.exports;
  const b = new TextEncoder().encode(JSON.stringify(input));
  const p = ex.synapse_alloc(b.length);
  new Uint8Array(ex.memory.buffer, p, b.length).set(b);
  const t0 = performance.now();
  const packed = BigInt.asUintN(64, ex.synapse_skill_invoke(p, b.length));
  const ms = (performance.now() - t0).toFixed(2);
  const raw = new TextDecoder().decode(new Uint8Array(ex.memory.buffer, Number(packed >> 32n), Number(packed & 0xffffffffn)));
  return { ms, out: JSON.parse(raw) };
}
const SA = "/home/user/stack-agent/";
const files = ["README.md","docs/architecture.md","docs/known-issues.md","docs/product/capability-sources.md","docs/product/experimentation.md","AGENTS.md"];
const sources = [];
for (const f of files) sources.push({ id: f, url: f, text: await readFile(SA + f, "utf8") });
const show = (label, r) => console.log(`\n## ${label} (${r.ms} ms)\n` + JSON.stringify(r.out, null, 1).slice(0, 1500));
const chunks = sources.flatMap(s => s.text.split(/\n\s*\n/).map((t, i) => ({ id: `${s.id}#${i}`, text: t })).filter(c => c.text.length > 80));
const r = await call("skill-04-citation-matcher", { claims: [
 "Guidance is pinned by sha256 digest and size-bounded.",
 "Production work can select a configuration that names a capability source.",
 "Production work cannot select a configuration that names a capability source.",
 "Stack-Agent loads plugins at runtime from a directory.",
 "The moon is made of green cheese."], passages: chunks.map(c => c.text) });
for (const m of r.out.result.matches) console.log(m.supported, m.score, chunks[m.passageIndex].id, "<-", m.claim);
