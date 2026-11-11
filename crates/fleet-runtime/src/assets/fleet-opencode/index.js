// Fleet's opencode plugin (an OpenCode 2 plugin directory). Fleet writes it
// under its own data directory and names it in OPENCODE_CONFIG_CONTENT,
// which opencode merges with the user's config: opencode's own config is
// never touched. This server part reports each change of state; the TUI
// part (tui.js) reports the start.
//
// Outside a session Fleet started, FLEET_SIGNAL_FILE is unset and the plugin
// registers nothing.
import { signal } from "./signal.js";

// The events that change the state. A subagent runs in a session of its
// own, so the agent works while any session executes, and waits on a person
// while any permission or question is open.
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

    // A subagent's session has a parent: its prompt is the agent's own, and
    // its session is not the one to resume.
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
        // A request is `id` when asked, `requestID` (a permission) or `id`
        // (a form) when answered; a form's own id is under `form`.
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
