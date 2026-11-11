// Fleet's omp extension. Fleet writes it under its own data directory and
// starts omp with `-e <this file>`: omp's own config is never touched.
//
// Each event runs `fleet signal <state>` with its JSON on stdin
// (docs/fleet/signal-contract.md). Outside a session Fleet started,
// FLEET_SIGNAL_FILE is unset and the extension registers nothing; a missing
// `fleet` never fails omp's turn.
//
// The file is plain JavaScript, which is also TypeScript: omp loads it as an
// extension, and any Node runs it in Fleet's tests.
import { spawn } from "node:child_process";

// One signal at a time, in the order they were asked for: two `fleet`
// processes started together may append in either order.
let last = Promise.resolve();

function signal(state, ctx, fields) {
  const payload = JSON.stringify({
    session_id: ctx.sessionManager.getSessionId(),
    ...fields,
  });
  last = last.then(() => send(state, payload));
  return last;
}

function send(state, payload) {
  return new Promise((resolve) => {
    try {
      const child = spawn("fleet", ["signal", state], {
        stdio: ["pipe", "ignore", "ignore"],
      });
      child.on("error", () => resolve());
      child.on("close", () => resolve());
      child.stdin.on("error", () => {});
      child.stdin.end(payload);
    } catch {
      resolve();
    }
  });
}

export default function (omp) {
  if (!process.env.FLEET_SIGNAL_FILE) return;
  // A subagent runs in omp's process with the same extensions, in a session
  // of its own and with no UI: its events are not the agent's.
  const on = (name, handler) =>
    omp.on(name, (event, ctx) => (ctx.hasUI ? handler(event, ctx) : undefined));
  // The tool calls waiting on a person's approval.
  const asking = new Set();
  const approval = (ctx) => signal(asking.size > 0 ? "blocked" : "working", ctx);

  on("session_start", (_event, ctx) => signal("idle", ctx));
  // A revive (`--restore`) goes on with an unfinished turn: no prompt.
  on("before_agent_start", (event, ctx) =>
    event.reason === "revive"
      ? undefined
      : signal("prompt", ctx, { prompt: event.prompt }),
  );
  on("agent_start", (_event, ctx) => signal("working", ctx));
  on("tool_approval_requested", (event, ctx) => {
    asking.add(event.toolCallId);
    return approval(ctx);
  });
  on("tool_approval_resolved", (event, ctx) => {
    asking.delete(event.toolCallId);
    return approval(ctx);
  });
  // A run that will continue (a retry, queued work) has not ended.
  on("agent_end", (event, ctx) => {
    if (event.willContinue) return undefined;
    asking.clear();
    return signal("idle", ctx);
  });
}
