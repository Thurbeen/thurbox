import { spawn } from "node:child_process";

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
  const on = (name, handler) =>
    omp.on(name, (event, ctx) => (ctx.hasUI ? handler(event, ctx) : undefined));
  const asking = new Set();
  const approval = (ctx) => signal(asking.size > 0 ? "blocked" : "working", ctx);

  on("session_start", (_event, ctx) => signal("idle", ctx));
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
  on("agent_end", (event, ctx) => {
    if (event.willContinue) return undefined;
    asking.clear();
    return signal("idle", ctx);
  });
}
