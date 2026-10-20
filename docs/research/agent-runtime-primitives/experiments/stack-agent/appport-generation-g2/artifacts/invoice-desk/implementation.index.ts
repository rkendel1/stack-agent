// DEVELOPER-OWNED after generation. AppPort never overwrites your edits.
import type { CapabilityImplementation } from "../generated/capability-boundary.js";
import { invokeExisting } from "../runtime/bridge.ts";

export const implementation: CapabilityImplementation = {
  "invoicedesk.invoices@1": async (request) => invokeExisting("http", "/api/invoices", "src/app.ts", request, {"method":"GET"}),
  "invoices.id@1": async (request) => invokeExisting("http", "/api/invoices/:id", "src/app.ts", request, {"method":"GET"}),
  "invoices.id.status@1": async (request) => invokeExisting("http", "/api/invoices/:id/status", "src/app.ts", request, {"method":"PATCH"})
};
