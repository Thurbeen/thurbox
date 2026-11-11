// The TUI part of Fleet's kilo plugin, named in KILO_TUI_CONFIG. It loads
// once the terminal UI is up, and so reports the start: the server part
// loads before the UI takes input, and a brief typed then is lost.
import { signal } from "./signal.js";

export default {
  id: "fleet.signal.tui",
  async tui() {
    if (!process.env.FLEET_SIGNAL_FILE) return;
    await signal("idle", {});
  },
};
