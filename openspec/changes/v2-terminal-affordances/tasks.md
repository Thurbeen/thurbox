## 1. Reads

- [x] 1.1 Publish the links visible in a session's terminal, including OSC 8 targets
- [x] 1.2 Publish whether a browser can be reached, so a plugin can say what opening will do

## 2. Commands

- [x] 2.1 Copy a session's visible terminal contents to the clipboard
- [x] 2.2 Open a link, falling back to copying it and reporting which happened
- [x] 2.3 Open a shell in a session's working directory

## 3. Notifications

- [x] 3.1 Raise a notification when a session becomes blocked, honouring the settings
- [x] 3.2 Suppress the notification for the session in view when configured
- [x] 3.3 Attempt no delivery at all when notifications are disabled

## 4. Plugins

- [x] 4.1 A links pane listing what is on screen and opening one
- [x] 4.2 A shell surface beside the agent in the centre slot
- [x] 4.3 Declare every key through the registry

## 6. Mouse, selection and paste

Promoted from a non-goal: the design said routing mouse events needed an event
model the kernel lacked. It turned out selection needs only *positions*, which
crossterm already delivers — no node-level event routing required.

- [x] 6.1 Move `ui::selection` to `session::selection`, so both halves share one implementation
- [x] 6.2 Enable mouse capture, and restore it on exit and on panic
- [x] 6.3 Drag-to-select over a terminal surface, clamped to that surface's rect
- [x] 6.4 Highlight the selection over the painted frame
- [x] 6.5 Copy the selection when there is one, else the whole visible screen
- [x] 6.6 Read selected text from the vt100 grid, so scrollback and soft-wrapped lines copy correctly
- [x] 6.7 Paste the clipboard into the focused terminal as a bracketed paste
- [x] 6.8 Reserve `ctrl+c` (only when a selection exists) and `ctrl+v`, so both work from any pane

## 5. Proof

- [x] 5.1 Copy reaches the clipboard, or reports the fallback
- [x] 5.2 Opening with no browser copies instead and says so
- [x] 5.3 A blocked session raises exactly one notification
- [x] 5.4 Disabled notifications attempt no delivery
