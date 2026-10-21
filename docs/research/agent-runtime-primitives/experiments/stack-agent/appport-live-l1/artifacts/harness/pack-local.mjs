// Packs AppBoundry's public packages (sdk, cli) from the working tree with the repo's own release staging,
// into a scratch directory. Nothing is written to the repository and nothing is published.
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { execFileSync } from "node:child_process";
const [repo, out] = process.argv.slice(2);
const { stageReleasePackage } = await import(join(repo, "scripts/release-stage.mjs"));
for (const dir of ["packages/sdk", "packages/cli"]) {
  const staged = join(out, "staged", dir.replaceAll("/", "-"));
  await mkdir(staged, { recursive: true });
  await stageReleasePackage(dir, staged);
  const report = JSON.parse(execFileSync("npm", ["pack", "--json", "--pack-destination", out], { cwd: staged, encoding: "utf8" }))[0];
  console.log(`${report.name}@${report.version} ${report.filename} sha512=${report.integrity}`);
}
