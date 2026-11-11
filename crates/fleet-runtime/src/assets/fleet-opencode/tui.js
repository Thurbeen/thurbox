// The TUI part of Fleet's opencode plugin. It loads once the terminal UI is
// up, and so reports the start: the server part loads before the UI takes
// input, and a brief typed then is lost.
import { signal } from "./signal.js";

export default {
  id: "fleet.signal.tui",
  async setup() {
    if (!process.env.FLEET_SIGNAL_FILE) return;
    await signal("idle", {});
  },
};
