// Fleet's kilo plugin. Fleet writes it under its own data directory and
// names it in KILO_CONFIG_CONTENT, which kilo merges with the user's config:
// kilo's own config is never touched. This server part reports each change
// of state; the TUI part (tui.js, named in KILO_TUI_CONFIG) reports the
// start.
//
// Outside a session Fleet started, FLEET_SIGNAL_FILE is unset and the plugin
// registers nothing.
import { signal } from "./signal.js";

// The requests that wait on a person, by the event that opens one and the
// events that close it: opened with `id`, closed with `requestID`.
const ASKED = new Set([
  "permission.asked",
  "question.asked",
  "suggestion.shown",
  "session.network.asked",
]);
const ANSWERED = new Set([
  "permission.replied",
  "question.replied",
  "question.rejected",
  "suggestion.accepted",
  "suggestion.dismissed",
  "session.network.replied",
  "session.network.rejected",
]);

export default {
  id: "fleet.signal",
  async server() {
    if (!process.env.FLEET_SIGNAL_FILE) return {};
    // A subagent runs in a session of its own, with a parent: the agent
    // works while any session is busy, and waits on a person while any
    // request is open.
    const running = new Set();
    const asking = new Set();
    const children = new Set();
    let conversation;
    const report = (state) =>
      signal(state, conversation ? { session_id: conversation } : {});

    return {
      async event({ event }) {
        const p = event.properties ?? {};
        if (event.type === "session.created" && p.info?.parentID) {
          children.add(p.info.id);
          return;
        }
        if (ASKED.has(event.type)) asking.add(p.id);
        else if (ANSWERED.has(event.type)) asking.delete(p.requestID);
        else if (event.type === "session.status") {
          // `busy` and `retry` run; `idle` ends the session's turn.
          if (p.status?.type === "idle") running.delete(p.sessionID);
          else running.add(p.sessionID);
        } else return;
        await report(
          asking.size > 0 ? "blocked" : running.size > 0 ? "working" : "idle",
        );
      },
      // A subagent's prompt is the agent's own, and its session is not the
      // one to resume.
      async "chat.message"(input, output) {
        if (children.has(input.sessionID)) return;
        conversation = input.sessionID;
        const prompt = (output.parts ?? [])
          .filter((part) => part.type === "text" && !part.synthetic)
          .map((part) => part.text)
          .join("\n");
        await signal("prompt", { session_id: input.sessionID, prompt });
      },
    };
  },
};
