# command-palette Specification

## Purpose
Makes every action the interface knows — a plugin's keys, its chord-less
commands, the kernel's own — reachable by name from one searchable modal, so an
action need not spend a chord to exist and a user need not remember one to run
it.
## Requirements
### Requirement: The palette is kernel chrome, opened by one global chord

The palette SHALL be a kernel-owned modal, like help and settings: not in the
arrangement, not in the focus ring, capturing input while open, closed by
`Esc`, one modal open at a time. It SHALL open on `ctrl+p` from every pane —
including a focused terminal — and its chord SHALL be rebindable like any
kernel chord.

#### Scenario: Opened from a focused terminal

- **WHEN** the agent pane is focused and `ctrl+p` is pressed
- **THEN** the palette opens and the keystroke does not reach the agent

#### Scenario: Another modal is open

- **WHEN** help is open and `ctrl+p` is pressed
- **THEN** help closes and the palette opens

#### Scenario: Closed without choosing

- **WHEN** the palette is open and `Esc` is pressed
- **THEN** it closes, focus returns to the pane that had it, and nothing runs

### Requirement: The palette lists every action in the registry

The palette SHALL list, from one source, every action the registry holds: each
plugin's declared keys, each plugin's declared chord-less commands, and the
kernel's own actions (the modals, reload, quit). Each row SHALL show the
action's description, its owning plugin, and its chord when it has one. A
disabled plugin's actions are absent, exactly as its keys are.

#### Scenario: A key-bound action appears

- **WHEN** a plugin declares `keys = { { key = "j", action = "mine.next",
  desc = "next item" } }`
- **THEN** the palette lists `next item` under that plugin with `j` beside it

#### Scenario: A chord-less command appears

- **WHEN** a plugin declares `commands = { { action = "mine.export", desc =
  "export the list" } }` and no key for it
- **THEN** the palette lists `export the list` with no chord

#### Scenario: A kernel action appears

- **WHEN** the palette is opened
- **THEN** reload, the settings, theme and help modals, and quit are listed
  under the kernel with their chords

#### Scenario: A disabled plugin

- **WHEN** a plugin is turned off in the Interface tab
- **THEN** none of its actions appear

### Requirement: Rows filter by fuzzy match and the selection survives refining

Typing SHALL filter rows by subsequence match against description, action id
and plugin name, using the same matcher the session list and search use, with
a live `matched/total` count; `Up`/`Down` move, `Enter` runs the selected row.
Refining the query SHALL keep the cursor on the same action when it survives
the filter.

#### Scenario: A query narrows the list

- **WHEN** `thm` is typed
- **THEN** rows whose description, id or plugin contain `t`, `h`, `m` in order
  remain, ranked as the shared matcher ranks them

#### Scenario: The cursor survives a refinement

- **WHEN** a row is selected and one more character is typed that the row
  still matches
- **THEN** that row stays selected

#### Scenario: Nothing matches

- **WHEN** the query matches no row
- **THEN** the palette says so and `Enter` does nothing

### Requirement: Running from the palette is indistinguishable from the key

`Enter` SHALL dispatch the selected action exactly as its chord would: a
plugin-scoped action reaches its plugin's `on_action` whether or not that
plugin is focused, a kernel action runs the kernel's handler, and the palette
closes first so the action sees the focus state it would have seen from a key
press. A plugin SHALL NOT be able to tell a palette dispatch from a key press.

#### Scenario: A plugin-scoped action from another pane

- **WHEN** the session list is focused, the palette is opened and the agent
  pane's `copy selection` is chosen
- **THEN** the agent pane's `on_action` receives that action

#### Scenario: A pane that opens its own column

- **WHEN** `open search` is chosen from the palette
- **THEN** the search strip opens and takes focus, exactly as `ctrl+/` does

#### Scenario: The action fails

- **WHEN** the chosen action's handler throws
- **THEN** the error is reported as a key-press failure would be, and the
  palette is already closed

### Requirement: A chord-less command is declared as data

A plugin SHALL declare chord-less commands as `commands = { { action, desc } }`
in its declaration table, with the same `action` namespace and the same
`on_action` handler its keys use. A command whose `action` is also bound by a
key is one row, not two. A command may later be given a chord by the user
through the existing rebinding surface.

#### Scenario: Declared and dispatched

- **WHEN** a plugin declares a command and it is chosen from the palette
- **THEN** its `on_action` is called with that action id

#### Scenario: Both a key and a command for one action

- **WHEN** a plugin declares `keys` and `commands` with the same `action`
- **THEN** the palette shows one row with the key's chord

#### Scenario: A user binds a chord to a command

- **WHEN** the user rebinds a chord-less command to `f7`
- **THEN** the chord fires the action, help lists it, and the palette shows
  `f7` beside the row

### Requirement: The palette never outranks what is reserved

While open the palette SHALL honour the reserved chords (`ctrl+q`, `f10`,
`ctrl+h`/`ctrl+l`, `f12`) and SHALL NOT list `ctrl+p`'s previous owner: the
chord is reassigned by this change and recorded as such in the keymap tests,
not silently reused.

#### Scenario: Quit from the palette

- **WHEN** the palette is open and `ctrl+q` is pressed
- **THEN** the interface quits

#### Scenario: The keymap ledger

- **WHEN** the keymap tests run
- **THEN** `ctrl+p` is asserted bound to the palette and is no longer in the
  list of chords awaiting a pane
