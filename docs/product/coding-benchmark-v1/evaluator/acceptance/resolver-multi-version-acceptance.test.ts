// Benchmark acceptance test (evaluator-held; not given to the agent).
//
// spec/appport-1.0.md §5: a server MAY support several versions of one
// capability name simultaneously. spec/dependencies-1.md: requirements carry a
// versionConstraint with the core resolver's semantics, and the resolver
// selects deterministically: highest version, then application ID.
import { describe, expect, it } from "vitest";
import {
  ApplicationDependencyResolver,
  ApplicationRegistry,
  MemoryApplicationStateStore,
  defineApplication,
  defineCapability,
  evaluateCompatibility,
  implementation
} from "@appport/core";
import { s } from "@appport/schema";

function invoicing(id: string, versions: number[]) {
  const provides = versions.map((version) =>
    defineCapability({
      name: "invoice.create",
      version,
      input: s.object({ amount: s.number() }),
      output: s.object({ id: s.string() }),
      handler: async () => ({ id: `${id}-v${version}` })
    })
  );
  const implementations = Object.fromEntries(
    versions.map((version) => [`invoice.create@${version}`, implementation(async () => ({ id: `${id}-v${version}` }))])
  );
  return defineApplication({
    identity: { id, name: `Invoicing ${id}`, version: "2.0.0" },
    provides,
    implementations
  });
}

async function registryWith(...apps: Array<[string, number[]]>) {
  const registry = new ApplicationRegistry(new MemoryApplicationStateStore());
  for (const [id, versions] of apps) {
    const app = invoicing(id, versions);
    await registry.register({
      identity: app.identity,
      manifest: app.manifest(),
      fingerprint: app.fingerprint(),
      verification: app.verify()
    });
  }
  return { registry, resolver: new ApplicationDependencyResolver(registry) };
}

async function resolve(apps: Array<[string, number[]]>, requirement: Record<string, unknown>) {
  const { resolver } = await registryWith(...apps);
  return resolver.resolveRequirement({ name: "invoice.create", ...requirement } as never);
}

function chosen(result: Awaited<ReturnType<typeof resolve>>) {
  return result.kind === "resolved" ? `${result.provider.id}@${result.capability.version}` : result.kind;
}

describe("a provider serving several versions of one capability", () => {
  it("is compatible with a requirement for any version it serves", async () => {
    const { registry } = await registryWith(["invoicing", [1, 2, 3]]);
    const provider = (await registry.get("invoicing"))!;
    expect(provider.manifest.provides.map((capability) => capability.version)).toEqual([1, 2, 3]);
    for (const constraint of ["1", "2", "3", "^2", ">=2", ">1", "<=2", "<3"]) {
      expect(
        evaluateCompatibility({ name: "invoice.create", version: 1, versionConstraint: constraint }, provider).compatible,
        constraint
      ).toBe(true);
    }
    for (const constraint of ["4", ">=4", "<1"]) {
      expect(
        evaluateCompatibility({ name: "invoice.create", version: 1, versionConstraint: constraint }, provider).compatible,
        constraint
      ).toBe(false);
    }
  });

  it("resolves an exact requirement to exactly that version", async () => {
    for (const version of [1, 2, 3]) {
      expect(chosen(await resolve([["invoicing", [1, 2, 3]]], { version, versionConstraint: String(version) }))).toBe(
        `invoicing@${version}`
      );
      expect(chosen(await resolve([["invoicing", [1, 2, 3]]], { version }))).toBe(`invoicing@${version}`);
    }
  });

  it("resolves a range to the highest version that satisfies it", async () => {
    const apps: Array<[string, number[]]> = [["invoicing", [1, 2, 3]]];
    expect(chosen(await resolve(apps, { version: 1, versionConstraint: ">=1" }))).toBe("invoicing@3");
    expect(chosen(await resolve(apps, { version: 1, versionConstraint: "<3" }))).toBe("invoicing@2");
    expect(chosen(await resolve(apps, { version: 2, versionConstraint: "^2" }))).toBe("invoicing@2");
  });

  it("resolves a provider-pinned requirement the same way", async () => {
    const apps: Array<[string, number[]]> = [["invoicing", [1, 2]]];
    expect(
      chosen(await resolve(apps, { version: 2, versionConstraint: "2", provider: { applicationId: "invoicing" } }))
    ).toBe("invoicing@2");
    expect(
      chosen(await resolve(apps, { version: 1, versionConstraint: "1", provider: { applicationId: "invoicing" } }))
    ).toBe("invoicing@1");
    expect(
      (await resolve(apps, { version: 3, versionConstraint: "3", provider: { applicationId: "invoicing" } })).kind
    ).toBe("unresolved");
  });

  it("is still unresolved when it serves no satisfying version", async () => {
    expect((await resolve([["invoicing", [1, 2]]], { version: 3, versionConstraint: "^3" })).kind).toBe("unresolved");
  });
});

describe("selection across providers", () => {
  it("picks the provider that serves the required version", async () => {
    expect(
      chosen(await resolve([["alpha", [1]], ["beta", [1, 2]]], { version: 2, versionConstraint: "2" }))
    ).toBe("beta@2");
  });

  it("compares providers by the version each would actually serve", async () => {
    // alpha's best satisfying version is 3; beta only serves 2.
    expect(
      chosen(await resolve([["alpha", [1, 3]], ["beta", [2]]], { version: 1, versionConstraint: ">=1" }))
    ).toBe("alpha@3");
    // Both serve 1 for an exact requirement: ambiguous, as before.
    expect((await resolve([["alpha", [1, 3]], ["beta", [1, 2]]], { version: 1, versionConstraint: "1" })).kind).toBe(
      "ambiguous"
    );
  });

  it("single-version providers behave as before", async () => {
    expect(chosen(await resolve([["alpha", [1]], ["beta", [2]]], { version: 1, versionConstraint: ">=1" }))).toBe(
      "beta@2"
    );
    expect(chosen(await resolve([["alpha", [1]], ["beta", [2]]], { version: 1, versionConstraint: "1" }))).toBe(
      "alpha@1"
    );
  });
});
