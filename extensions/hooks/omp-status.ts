// Managed by thurbox `extension install` (the built-in "hooks" extension).
// Reinstalling or updating overwrites this file — do not edit; uninstalling
// removes it. Reports Oh My Pi's lifecycle state to thurbox. Identity comes
// from the inherited $THURBOX_SESSION env var; every call is best-effort so it
// can never break a session running outside thurbox.
//
// This is an OMP (Oh My Pi, https://github.com/can1357/oh-my-pi) extension
// (TypeScript), auto-discovered from ~/.omp/agent/extensions/*.ts by the omp
// CLI. It subscribes to OMP's lifecycle events and reports them to thurbox's
// status reporter:
//   session_start → idle, agent_start + tool_execution_start +
//   tool_execution_end → working
//   (a structured user-question tool call → blocked), agent_end → done.
//
// OMP is Pi-compatible, but its structured user-question tool is named `ask`
// (upstream pi uses `ask_user_question`). We treat BOTH as blocking so the dot
// turns red while OMP waits for the user instead of staying yellow (working).
import { exec } from "node:child_process";
import type { ExtensionAPI } from "@oh-my-pi/pi-coding-agent";

// Tool names that block the turn until the user answers.
const BLOCKING_TOOLS = new Set(["ask", "ask_user_question"]);

// Exact marker prefix kept on one line so the remote (SSH/WSL) rewrite can swap
// this command for a tmux pane-option setter (there is no thurbox-cli on a
// remote host). Do not split the words across lines or reorder the flags.
const SIGNAL = "thurbox-cli session signal --state ";

// Fire-and-forget; the callback swallows errors so a hook never surfaces into
// the agent. exec inherits the OMP process env, so $THURBOX_SESSION travels.
const report = (state: string): void => {
  exec(SIGNAL + state, () => {});
};

// `pi` is injected by the OMP runtime; the `import type` above is erased at
// runtime, so this file has no hard dependency beyond Node's built-ins.
export default function (pi: ExtensionAPI): void {
  pi.on("session_start", () => report("idle"));
  pi.on("agent_start", () => report("working"));
  pi.on("tool_execution_start", (event?: { toolName?: string }) => {
    // A structured question to the user blocks the turn until it is answered;
    // any other tool call means the agent is actively working.
    report(BLOCKING_TOOLS.has(event?.toolName ?? "") ? "blocked" : "working");
  });
  // The blocking tool finishing is the user's answer arriving: there is no
  // separate "answered" event, so without this the dot stays red until the
  // next tool call, or to the end of a turn that asked nothing more.
  pi.on("tool_execution_end", () => report("working"));
  pi.on("agent_end", () => report("done"));
}
