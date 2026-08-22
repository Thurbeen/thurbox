## 1. The modal layer

- [x] 1.1 A kernel-owned modal surface, drawn above the arrangement and above
      plugin floats
- [x] 1.2 One modal at a time; opening one closes another
- [x] 1.3 A modal captures input while open — no plugin is offered a key
- [x] 1.4 `Esc` closes; the opening chord toggles
- [x] 1.5 A modal is never in the layout and never in the focus ring

## 2. Help

- [x] 2.1 Render every declared binding, grouped by its declared group
- [x] 2.2 Show the chord as v1 spells it (`ctrl+b / f2`), one row per action
- [x] 2.3 List the reserved chords as a fixed, unrebindable section
- [x] 2.4 Capture a chord onto the selected action, including chords the kernel
      otherwise reserves
- [x] 2.5 Reassign a chord already bound elsewhere, and say what moved
- [x] 2.6 Reset one action to its default; reset every action
- [x] 2.7 Persist through the registry, effective on the next keystroke

## 3. Settings

- [x] 3.1 Render every declared setting, grouped by the declaring plugin
- [x] 3.2 Edit a bool, a number and a text value
- [x] 3.3 Reset a setting to its default
- [x] 3.4 Write back through the registry and persist
- [x] 3.5 Declare real settings on the bundled plugins, so the modal has content

## 4. Theme

- [x] 4.1 Render the presets and user themes, grouped dark/light
- [x] 4.2 Filter by name and id
- [x] 4.3 Apply on selection and persist, exactly where v1 persists it
- [x] 4.4 Take no plugin contribution

## 5. The centre pane

- [x] 5.1 One `terminal` plugin owning the agent and shell tabs
- [x] 5.2 The tab strip renders on every tab, not only the agent's
- [x] 5.3 The pane is a single focus stop
- [x] 5.4 Room for review as a third tab, without implementing it

## 6. Proof

- [x] 6.1 `Tab` from the terminal reaches the session list in one press
- [x] 6.2 A plugin that declares a key appears in help without touching help
- [x] 6.3 A plugin that declares a setting appears in settings without touching
      settings
- [x] 6.4 Capturing `ctrl+q` in help reaches the modal instead of quitting
      — the safety property. v2 then REFUSES it ("ctrl+q is reserved and cannot
      be rebound") where v1 would have bound it: the kernel keeps a fixed escape
      hatch so a capture cannot lock you out of the application. Deliberate
      divergence from v1, recorded rather than silently accepted.
- [x] 6.5 A modal covers the terminal rather than replacing it
- [x] 6.6 Opening a modal does not move focus, and closing it restores nothing
      because nothing moved
