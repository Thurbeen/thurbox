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
