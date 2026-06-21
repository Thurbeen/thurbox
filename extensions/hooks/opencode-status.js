// Managed by thurbox `extension install` (the built-in "hooks" extension).
// Reinstalling or updating overwrites this file — do not edit; uninstalling
// removes it. Reports opencode's lifecycle state to thurbox via
// `thurbox-cli session signal`. Identity comes from the inherited
// $THURBOX_SESSION env var; every call is best-effort so it can never break a
// session running outside thurbox.
export const ThurboxStatus = async ({ $ }) => {
  const signal = async (state) => {
    try {
      await $`thurbox-cli session signal --state ${state}`.quiet().nothrow();
    } catch (_) {
      // best-effort: never surface hook errors into the agent
    }
  };
  return {
    "chat.message": async () => {
      await signal("working");
    },
    event: async ({ event }) => {
      if (!event || !event.type) return;
      if (event.type === "session.created") await signal("idle");
      else if (event.type === "permission.asked") await signal("blocked");
      else if (event.type === "session.idle") await signal("done");
    },
  };
};
