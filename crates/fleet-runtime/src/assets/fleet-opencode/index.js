import { signal } from "./signal.js";

const ASKED = new Set(["permission.asked", "form.created"]);
const ANSWERED = new Set([
  "permission.replied",
  "permission.rejected",
  "form.replied",
  "form.cancelled",
]);
const STARTED = new Set(["session.execution.started"]);
const ENDED = new Set([
  "session.execution.succeeded",
  "session.execution.failed",
  "session.execution.interrupted",
]);

export default {
  id: "fleet.signal",
  async setup(ctx) {
    if (!process.env.FLEET_SIGNAL_FILE) return;
    const running = new Set();
    const asking = new Set();
    let conversation;
    const report = (state) =>
      signal(state, conversation ? { session_id: conversation } : {});

    await ctx.session.hook("prompt", async (event) => {
      const info = await ctx.session
        .get({ sessionID: event.sessionID })
        .catch(() => undefined);
      if (info?.parentID) return;
      conversation = event.sessionID;
      await signal("prompt", {
        session_id: event.sessionID,
        prompt: event.prompt.text,
      });
    });

    const controller = new AbortController();
    const types = [...ASKED, ...ANSWERED, ...STARTED, ...ENDED];
    void (async () => {
      for await (const event of ctx.event.subscribe({
        types,
        signal: controller.signal,
      })) {
        const data = event.data ?? {};
        const request = data.form?.id ?? data.requestID ?? data.id;
        if (ASKED.has(event.type)) asking.add(request);
        else if (ANSWERED.has(event.type)) asking.delete(request);
        else if (STARTED.has(event.type)) running.add(data.sessionID);
        else if (ENDED.has(event.type)) running.delete(data.sessionID);
        await report(
          asking.size > 0 ? "blocked" : running.size > 0 ? "working" : "idle",
        );
      }
    })().catch(() => {});
    return () => controller.abort();
  },
};
