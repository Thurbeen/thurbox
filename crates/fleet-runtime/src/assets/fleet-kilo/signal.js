// `fleet signal <state>` with `fields` as the JSON on its stdin
// (docs/fleet/signal-contract.md). A missing or failing `fleet` never fails
// kilo's turn.
//
// One signal at a time, in the order they were asked for: kilo does not wait
// for one event's handler before it runs the next, and two `fleet` processes
// started together may append in either order.
import { spawn } from "node:child_process";

let last = Promise.resolve();

export function signal(state, fields) {
  last = last.then(() => send(state, fields));
  return last;
}

function send(state, fields) {
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
