import { signal } from "./signal.js";

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
          if (p.status?.type === "idle") running.delete(p.sessionID);
          else running.add(p.sessionID);
        } else return;
        await report(
          asking.size > 0 ? "blocked" : running.size > 0 ? "working" : "idle",
        );
      },
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
