import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
const SYN = "/home/user/synapse/";
const inv = JSON.parse(readFileSync(SYN + "synapse.inventory.json", "utf8"));
const cache = new Map();
export async function call(mod, input) {
  if (!cache.has(mod)) {
    const m = inv.modules.find(x => x.module_id === mod);
    const bytes = readFileSync(SYN + m.artifact.path);
    if (createHash("sha256").update(bytes).digest("hex") !== m.artifact.sha256) throw new Error("sha " + mod);
    cache.set(mod, await WebAssembly.compile(bytes));
  }
  const instance = await WebAssembly.instantiate(cache.get(mod), {});
  const ex = instance.exports;
  const b = new TextEncoder().encode(JSON.stringify(input));
  const p = ex.synapse_alloc(b.length);
  new Uint8Array(ex.memory.buffer, p, b.length).set(b);
  const packed = BigInt.asUintN(64, ex.synapse_skill_invoke(p, b.length));
  return JSON.parse(new TextDecoder().decode(new Uint8Array(ex.memory.buffer, Number(packed >> 32n), Number(packed & 0xffffffffn))));
}
