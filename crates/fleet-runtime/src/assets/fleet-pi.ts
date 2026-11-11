// Fleet's pi extension. Fleet writes it under its own data directory and
// starts pi with `-e <this file>`: pi's own config is never touched.
//
// Each event runs `fleet signal <state>` with its JSON on stdin
// (docs/fleet/signal-contract.md). Outside a session Fleet started,
// FLEET_SIGNAL_FILE is unset and the extension registers nothing; a missing
// `fleet` never fails pi's turn.
//
// The file is plain JavaScript, which is also TypeScript: pi loads it as an
// extension, and any Node runs it in Fleet's tests.
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
  // pi asks a person nothing of its own, so it reports no blocked state.
  // `agent_settled`, not `agent_end`: pi may still retry, compact or take a
  // queued message after a run ends.
  pi.on("session_start", (_event, ctx) => signal("idle", ctx));
  pi.on("before_agent_start", (event, ctx) =>
    signal("prompt", ctx, { prompt: event.prompt }),
  );
  pi.on("agent_start", (_event, ctx) => signal("working", ctx));
  pi.on("agent_settled", (_event, ctx) => signal("idle", ctx));
}
