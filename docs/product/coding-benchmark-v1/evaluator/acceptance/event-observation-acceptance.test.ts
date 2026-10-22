// Benchmark acceptance test (evaluator-held; not given to the agent).
//
// spec/appport-1.0.md §9.4 / §21.7: subscriptions are authorized; an event a
// session may not observe is not delivered to it, and `event.name` MAY be `*`
// meaning "every event the session may observe". Exercised through a server
// connection and through the HTTP Server-Sent Events endpoint.
import { describe, expect, it } from "vitest";
import type { ServerEnvelope, Session } from "@appport/protocol";
import { permissionAuthorizer } from "@appport/authorization";
import { s } from "@appport/schema";
import { createApplication, defineEvent } from "@appport/core";
import { createHttpBinding, createServer } from "@appport/server";

const now = new Date().toISOString();
const APP = "dev.appport.benchmark.events";

function session(id: string, permissions: string[]): Session {
  return { id, applicationId: APP, createdAt: now, principal: { id: `user_${id}`, type: "user" }, permissions };
}

const sessions: Record<string, Session> = {
  ses_reader: session("ses_reader", ["documents.read"]),
  ses_auditor: session("ses_auditor", ["audit.read"]),
  ses_nothing: session("ses_nothing", []),
  ses_wildcard: session("ses_wildcard", ["documents.*", "audit.*"])
};

function setup() {
  const application = createApplication({
    application: { id: APP, name: "Event benchmark", version: "1.0.0", description: "Benchmark fixture." },
    capabilities: [],
    events: [
      defineEvent({
        name: "documents.changed",
        version: 1,
        description: "A document changed.",
        payload: s.object({ documentId: s.string() }),
        authorization: ["documents.read"]
      }),
      defineEvent({
        name: "audit.recorded",
        version: 1,
        description: "An audit record.",
        payload: s.object({ entry: s.string() }),
        authorization: ["audit.read"]
      }),
      defineEvent({
        name: "status.heartbeat",
        version: 1,
        description: "Public heartbeat.",
        payload: s.object({ at: s.string() })
      })
    ],
    authorizer: permissionAuthorizer(),
    mode: "production"
  });
  const server = createServer({
    application,
    authenticate: ({ sessionId }) => {
      const found = sessionId ? sessions[sessionId] : undefined;
      return found ? { session: found, principal: found.principal } : {};
    }
  });
  const emitAll = async () => {
    await application.emit({ name: "documents.changed", payload: { documentId: "doc_secret" } });
    await application.emit({ name: "audit.recorded", payload: { entry: "audit_secret" } });
    await application.emit({ name: "status.heartbeat", payload: { at: now } });
    await new Promise((resolve) => setTimeout(resolve, 20));
  };
  return { application, server, emitAll };
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 20));

async function overConnection(sessionId: string | undefined, eventName: string) {
  const { server, emitAll } = setup();
  const sent: ServerEnvelope[] = [];
  const identity = await server.identify({ transport: "websocket", ...(sessionId ? { sessionId } : {}) });
  const connection = server.createConnection({ transport: "websocket", identity, send: (envelope) => void sent.push(envelope) });
  await connection.acceptValue({
    protocol: "appport/1",
    type: "subscribe",
    requestId: "req_sub",
    subscriptionId: "sub_1",
    event: { name: eventName }
  });
  await settle();
  await emitAll();
  return {
    events: sent.flatMap((envelope) => (envelope.type === "event" ? [envelope.event.name] : [])).sort(),
    raw: JSON.stringify(sent)
  };
}

async function overEventStream(sessionId: string | undefined, query?: string) {
  const { server, emitAll } = setup();
  const binding = createHttpBinding({ server });
  const abort = new AbortController();
  const outcome = await binding.route({
    method: "GET",
    path: "/appport/events",
    query: new URLSearchParams(query ?? ""),
    headers: sessionId ? { "x-appport-session": sessionId } : {},
    signal: abort.signal
  });
  expect(outcome.kind).toBe("stream");
  if (outcome.kind !== "stream") throw new Error("no stream");
  const frames: string[] = [];
  const reading = (async () => {
    for await (const frame of outcome.frames) frames.push(frame);
  })();
  await settle();
  await emitAll();
  outcome.cancel();
  abort.abort();
  await Promise.race([reading, settle()]);
  const text = frames.join("");
  const events = [...text.matchAll(/^event: (.+)$/gm)].map((match) => match[1]!).sort();
  return { events, raw: text };
}

describe("wildcard subscriptions deliver only what the session may observe", () => {
  it("an anonymous connection subscribed to * receives only public events", async () => {
    const { events, raw } = await overConnection(undefined, "*");
    expect(events).toEqual(["status.heartbeat"]);
    expect(raw).not.toContain("doc_secret");
    expect(raw).not.toContain("audit_secret");
  });

  it("an authenticated session without the permission receives only public events", async () => {
    const { events, raw } = await overConnection("ses_nothing", "*");
    expect(events).toEqual(["status.heartbeat"]);
    expect(raw).not.toContain("doc_secret");
  });

  it("each session receives exactly the protected events it holds permission for", async () => {
    expect((await overConnection("ses_reader", "*")).events).toEqual(["documents.changed", "status.heartbeat"]);
    expect((await overConnection("ses_auditor", "*")).events).toEqual(["audit.recorded", "status.heartbeat"]);
  });

  it("wildcard permissions grant observation as they grant everything else", async () => {
    expect((await overConnection("ses_wildcard", "*")).events).toEqual([
      "audit.recorded",
      "documents.changed",
      "status.heartbeat"
    ]);
  });

  it("named subscriptions keep their behaviour", async () => {
    expect((await overConnection("ses_reader", "documents.changed")).events).toEqual(["documents.changed"]);
    expect((await overConnection(undefined, "status.heartbeat")).events).toEqual(["status.heartbeat"]);
    const refused = await overConnection(undefined, "documents.changed");
    expect(refused.events).toEqual([]);
    expect(refused.raw).toContain("UNAUTHORIZED");
    const forbidden = await overConnection("ses_nothing", "documents.changed");
    expect(forbidden.events).toEqual([]);
    expect(forbidden.raw).toContain("FORBIDDEN");
  });
});

describe("the HTTP event stream", () => {
  it("an anonymous stream that names no events carries only public events", async () => {
    const { events, raw } = await overEventStream(undefined);
    expect(events).toEqual(["status.heartbeat"]);
    expect(raw).not.toContain("doc_secret");
    expect(raw).not.toContain("audit_secret");
  });

  it("an anonymous stream asking for * carries only public events", async () => {
    const { events, raw } = await overEventStream(undefined, "events=*");
    expect(events).toEqual(["status.heartbeat"]);
    expect(raw).not.toContain("doc_secret");
  });

  it("a permitted session's stream carries its protected events", async () => {
    expect((await overEventStream("ses_reader")).events).toEqual(["documents.changed", "status.heartbeat"]);
    expect((await overEventStream("ses_reader", "events=documents.changed")).events).toEqual(["documents.changed"]);
  });
});
