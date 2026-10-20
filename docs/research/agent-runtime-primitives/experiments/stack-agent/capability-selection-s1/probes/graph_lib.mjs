import { readFile } from "node:fs/promises";
import { createHash } from "node:crypto";
const S = "/home/user/synapse/components/capabilities/";
const PIN = {
  "repository-graph-builder": "52271be821a35610c78eed12ef1d57e484976411d57412f98374ecff4fdf29d0",
  "repository-impact-traverser": "83d03c809bf3635a50a48424fefff347468bd7ba2256ba35ddad79fc8d277c38",
  "graph-impact-differ": "c31f63a1f11f9653208272195d10a3049888eaad7f06696f9a46a91c9cb556af",
  "repository-graph-differ": "7664d0532677fed50116b9b9258b0ca97f195d3c6c7211e61f1208f2c65ff377",
};
export async function call(mod, input) {
  const bytes = await readFile(S + mod + "/src/browser_agent.wasm");
  const h = createHash("sha256").update(bytes).digest("hex");
  if (h !== PIN[mod]) throw new Error(`${mod} sha mismatch ${h}`);
  const { instance } = await WebAssembly.instantiate(bytes, {});
  const ex = instance.exports;
  const b = new TextEncoder().encode(JSON.stringify(input));
  const p = ex.synapse_alloc(b.length);
  new Uint8Array(ex.memory.buffer, p, b.length).set(b);
  const packed = BigInt.asUintN(64, ex.synapse_skill_invoke(p, b.length));
  const out = JSON.parse(new TextDecoder().decode(new Uint8Array(ex.memory.buffer, Number(packed >> 32n), Number(packed & 0xffffffffn))));
  if (!out.ok) throw new Error(`${mod}: ${JSON.stringify(out)}`);
  return out.result;
}
