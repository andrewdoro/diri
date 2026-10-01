// Diri telemetry Worker: ingest, admin API, retention. See README.md and
// diri/TELEMETRY.md (Upload, Worker) for the contract.

import { handleAdmin } from "./admin";
import { type Env, error, json } from "./env";
import { handleIngest } from "./ingest";
import { sweep } from "./retention";

export type { Env };

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/v1/ingest") return handleIngest(request, env);
    if (url.pathname.startsWith("/v1/admin/")) return handleAdmin(request, env, url);
    if (url.pathname === "/healthz") return json({ ok: true });
    return error(404, "not_found");
  },

  async scheduled(_controller: ScheduledController, env: Env, ctx: ExecutionContext): Promise<void> {
    ctx.waitUntil(
      sweep(env).then((report) => {
        console.log(JSON.stringify({ event: "retention", ...report }));
      }),
    );
  },
} satisfies ExportedHandler<Env>;
