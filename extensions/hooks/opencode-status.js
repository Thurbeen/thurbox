// Managed by thurbox `extension install` (the built-in "hooks" extension).
// Reinstalling or updating overwrites this file — do not edit; uninstalling
// removes it. Reports opencode's lifecycle state to thurbox via
// `thurbox-cli session signal`. Identity comes from the inherited
// $THURBOX_SESSION env var; every call is best-effort so it can never break a
// session running outside thurbox.
//
// One module for both plugin APIs opencode ships, because both lines are
// released: 1.x reads `server` off the default export, 2.x refuses any module
// whose default export lacks `id` + `setup` (issue #1417). Each host ignores
// the other's key — except that 1.x also calls `setup` through a partial 2.x
// bridge with no `event` domain, which `setup` returns early on.
import { exec } from "node:child_process";

// Exact marker prefix kept on one line so the remote (SSH/WSL) rewrite can swap
// this command for its backend's status setter (there is no thurbox-cli on a
// remote host). Do not split the words across lines or reorder the flags.
const SIGNAL = "thurbox-cli session signal --state ";

// Resolves once the signal has run, so awaiting callers report in order; the
// callback swallows errors so a hook never surfaces into the agent.
const signal = (state) =>
  new Promise((resolve) => {
    try {
      exec(SIGNAL + state, () => resolve());
    } catch (_) {
      resolve();
    }
  });

export default {
  id: "thurbox-status",

  // opencode 1.x.
  server: async () => ({
    "chat.message": async () => {
      await signal("working");
    },
    event: async ({ event }) => {
      if (!event || !event.type) return;
      if (event.type === "session.created") await signal("idle");
      else if (event.type === "permission.asked") await signal("blocked");
      // Allowed or denied, the turn is opencode's again — without this the
      // dot stays red for the whole tool run, and to the end of a turn whose
      // last permission was the last thing it asked for.
      else if (event.type === "permission.replied") await signal("working");
      else if (event.type === "session.idle") await signal("done");
    },
  }),

  // opencode 2.x. Every session the server runs reaches this one stream, and a
  // subagent runs in a child session of its own: its creation and its end are
  // not this session's, so they are skipped rather than reported as idle/done
  // while the parent is still working. Its permission prompts still block.
  //
  // The shared background server (`serve --service`) is started with the
  // environment of whichever opencode launched it first, so its
  // $THURBOX_SESSION is no particular session's: it reports nothing. Only a
  // `--standalone` server, spawned by the session's own TUI, does.
  setup: (ctx) => {
    if (!ctx.event || typeof ctx.event.subscribe !== "function") return;
    if (process.argv.includes("--service")) return;
    const stop = new AbortController();
    const children = new Set();
    (async () => {
      for await (const event of ctx.event.subscribe({ signal: stop.signal })) {
        const data = (event && event.data) || {};
        switch (event && event.type) {
          case "session.created":
            if (data.parentID) children.add(data.sessionID);
            else await signal("idle");
            break;
          case "session.execution.started":
          case "permission.replied":
            await signal("working");
            break;
          case "permission.asked":
            await signal("blocked");
            break;
          case "session.execution.succeeded":
          case "session.execution.failed":
          case "session.execution.interrupted":
            if (!children.has(data.sessionID)) await signal("done");
            break;
        }
      }
    })().catch(() => {});
    return () => stop.abort();
  },
};
