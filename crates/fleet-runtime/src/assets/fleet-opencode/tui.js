import { signal } from "./signal.js";

export default {
  id: "fleet.signal.tui",
  async setup() {
    if (!process.env.FLEET_SIGNAL_FILE) return;
    await signal("idle", {});
  },
};
