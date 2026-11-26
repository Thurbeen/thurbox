import { signal } from "./signal.js";

export default {
  id: "fleet.signal.tui",
  async tui() {
    if (!process.env.FLEET_SIGNAL_FILE) return;
    await signal("idle", {});
  },
};
