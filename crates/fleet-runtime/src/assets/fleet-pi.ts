import { spawn } from "node:child_process";

function signal(state, ctx, fields) {
  const payload = JSON.stringify({
    session_id: ctx.sessionManager.getSessionId(),
    ...fields,
  });
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

export default function (pi) {
  if (!process.env.FLEET_SIGNAL_FILE) return;
  pi.on("session_start", (_event, ctx) => signal("idle", ctx));
  pi.on("before_agent_start", (event, ctx) =>
    signal("prompt", ctx, { prompt: event.prompt }),
  );
  pi.on("agent_start", (_event, ctx) => signal("working", ctx));
  pi.on("agent_settled", (_event, ctx) => signal("idle", ctx));
}
