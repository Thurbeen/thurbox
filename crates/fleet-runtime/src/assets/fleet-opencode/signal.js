// `fleet signal <state>` with `fields` as the JSON on its stdin
// (docs/fleet/signal-contract.md). A missing or failing `fleet` never fails
// opencode's turn.
import { spawn } from "node:child_process";

export function signal(state, fields) {
  return new Promise((resolve) => {
    try {
      const child = spawn("fleet", ["signal", state], {
        stdio: ["pipe", "ignore", "ignore"],
      });
      child.on("error", () => resolve());
      child.on("close", () => resolve());
      child.stdin.on("error", () => {});
      child.stdin.end(JSON.stringify(fields));
    } catch {
      resolve();
    }
  });
}
