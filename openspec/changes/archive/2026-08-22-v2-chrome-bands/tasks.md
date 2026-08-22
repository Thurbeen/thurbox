## 1. A slot the kernel occupies

- [x] 1.1 Let the layout resolver place a named region whose occupant supplies no
      plugin tree, and leave an unplaced region undrawn rather than an error
- [x] 1.2 Route a resolved band region to the band renderer during paint, before
      floats and modals
- [x] 1.3 Keep bands out of the focus ring and out of key dispatch, asserted
      rather than assumed

## 2. The bands

- [x] 2.1 The identity band: product name, tagline, running version, active theme,
      selected session, and the newer-version notice when one is known
- [x] 2.2 The message band: one message at informational / success / error
      severity, visibly distinguished, occupying a row only while a message
      stands and releasing it on expiry
- [x] 2.3 The action band: the focused surface's name, the live counts, and the
      entries — each showing the chord actually in force
- [x] 2.4 Width pressure: entries dropped lowest-priority-first, never truncated
      or overlapped
- [x] 2.5 Height pressure: identity dropped first, then action, message last;
      the pane area never falls below one row

## 3. Contributed entries

- [x] 3.1 Collect a third declaration kind (`pills`) alongside keys and settings,
      stamped with the declaring plugin and enumerable without invoking it
- [x] 3.2 Order entries by declared priority, resolving each one's current chord
      through the registry so a rebind is reflected
- [x] 3.3 Drop an entry whose action is declared nowhere, so no band offers an
      affordance that would do nothing
- [x] 3.4 Activating an entry runs the same action its chord runs, through the
      same path

## 4. Placement, and retiring the old status row

- [x] 4.1 Place the three bands in the bundled `ui/layout.lua`
- [x] 4.2 Remove the status-message special case that paints onto the centre
      pane's bottom border, in the same step that places the message band
- [x] 4.3 Declare the bundled panes' entries as `pills`, replacing whatever the
      action band would otherwise hardcode
- [x] 4.4 Carry long-running progress on the surface chosen for it, and confirm
      it outlives the message retention window

## 5. Proof

- [x] 5.1 Each band's content is asserted element by element against what v1
      shows for the same state — brand, version, theme, session, severity badges,
      entry labels and their chords. NOT cell-exact against the v1 recordings:
      v2's action band carries a different entry set, so a byte comparison would
      assert the old pill list rather than the behaviour
- [x] 5.2 A plugin that throws while rendering leaves every band drawing with its
      entries intact; a plugin that becomes unloadable leaves the last good set
      in force and reports the failure
- [x] 5.3 An arrangement omitting a band drops it and nothing else; an
      arrangement reordering bands moves them with their contents intact
- [x] 5.4 A plugin added with a `pills` declaration gains an entry with no other
      file edited; removing it retires the entry
- [x] 5.5 A rebound action's entry shows the new chord
- [x] 5.6 A message appears at its severity, expires, and releases its row
- [x] 5.7 Focus cycling never lands on a band
- [x] 5.8 `v2-parity-gaps` has its `status bar` and `header band` rows struck,
      and `tests/v2_parity.rs` no longer lists either as awaiting a plugin
