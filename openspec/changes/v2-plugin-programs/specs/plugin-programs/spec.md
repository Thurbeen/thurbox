## ADDED Requirements

### Requirement: A plugin can put a running program in a pane it owns

The system SHALL let a plugin ask for a named program to run, keep it running, and
paint its screen into the rect the plugin is drawing into. The pane SHALL belong to
the **plugin** rather than to a session, so it exists independently of which
session is selected and is not lost when the selection moves.

Asking SHALL be idempotent: a plugin that asks for the same pane on every frame
SHALL get the pane it already has, not a second copy of the program. This mirrors
the pattern already established for asking a program for its output, so a plugin
author learns one rule rather than two.

The pane SHALL be addressed the way a session's companion terminal already is — as
an identifier the kernel resolves — so that showing one needs no new kind of node.

#### Scenario: A plugin asks for a program

- **WHEN** a plugin asks for a named program to run
- **THEN** the program is started and its screen is painted into the plugin's rect

#### Scenario: The plugin asks again

- **WHEN** a plugin asks for a pane it already has
- **THEN** the existing pane is kept and no second copy of the program is started

#### Scenario: The selection moves

- **WHEN** the selected session changes while a plugin's pane is running
- **THEN** the pane and the program in it are unaffected

#### Scenario: A pane nothing has asked for

- **WHEN** a plugin draws a pane it has not asked to start
- **THEN** it is reported as having nothing behind it rather than drawn as though
  it were running

### Requirement: Running a program in a pane is a capability the user grants

The system SHALL treat putting a program in a pane as a **distinct** capability
from asking a program for its output, and SHALL withhold it until the user has
trusted that file. A plugin declaring it and not yet trusted SHALL load, draw and
declare its keys as usual, with the capability simply absent from it.

The two SHALL NOT share a grant. A file trusted to ask for a program's output has
been trusted for something bounded — a capped amount of output, a timeout, a limit
on how many run at once. An interactive program has none of those bounds, and
holds the user's keystrokes as well. Treating one grant as covering both would
widen what the user agreed to without asking them.

#### Scenario: An untrusted plugin cannot start a program

- **WHEN** a plugin declares the capability and the user has not trusted the file
- **THEN** the plugin still loads and draws
- **AND** no program is started for it

#### Scenario: A plugin can tell whether it has been granted this

- **WHEN** a plugin declares the capability
- **THEN** it can read whether that capability has been granted to it
- **AND** it can therefore draw the untrusted state deliberately, rather than
  being unable to distinguish "not trusted" from "not started yet"

This is stated because withholding by absence — the way a capability that is a
function is withheld — cannot express this one: asking goes through a facility
every plugin has.

#### Scenario: Trust for output does not confer trust for a pane

- **WHEN** a file is trusted to ask for a program's output
- **THEN** it still may not put a program in a pane until trusted for that

#### Scenario: A trusted plugin may start a program

- **WHEN** the user has trusted a file that declares the capability
- **THEN** that plugin may start a program, and no other plugin gains the ability

#### Scenario: What a file asks for is visible before granting it

- **WHEN** a file declaring the capability is listed
- **THEN** it is reported as asking to run a program the user interacts with,
  distinguishably from one that only asks for a program's output
- **AND** this is reported without reading the file's source

### Requirement: Keystrokes reach the program the focused pane is showing

The system SHALL forward keys the focused plugin does not handle to the program its
own surface names, rather than to a session the kernel selected. A plugin SHALL
declare that it wants raw input; where those keys go SHALL follow the tree the
plugin returned.

Chords that are the way *out* of a pane SHALL NOT be forwarded, so a program that
consumes every key cannot trap the user inside it.

#### Scenario: A key reaches the program

- **WHEN** the focused plugin wants raw input, shows a program pane, and does not
  handle a key
- **THEN** the key is delivered to that program

#### Scenario: The escape route is never forwarded

- **WHEN** a chord that moves focus or quits is pressed while a program pane is
  focused
- **THEN** it is handled by the interface rather than delivered to the program

#### Scenario: An unfocused pane receives nothing

- **WHEN** a plugin holding a program pane does not have focus
- **THEN** keys are not delivered to its program

#### Scenario: The pane is not running

- **WHEN** the focused plugin wants raw input and its pane has no program behind it
- **THEN** the key is not delivered anywhere, and is not silently treated as
  handled

### Requirement: A program is told the size it is drawn at

The system SHALL size the program to the rect it is painted into, and SHALL tell it
when that rect changes — so a full-screen program is not drawn into a window of the
wrong shape.

The pane SHALL be started at the size of the rect it will be painted into where
that is known, rather than at the size of the whole terminal.

#### Scenario: The rect changes

- **WHEN** the rect a program pane is painted into changes size
- **THEN** the program is resized to match

#### Scenario: The rect does not change

- **WHEN** a frame is painted and the rect is unchanged
- **THEN** the program is not resized

### Requirement: A pane's lifetime is stated, not incidental

The system SHALL define what happens to a running program when the interface
reloads, when it exits, and when the program itself ends:

- Reloading the interface SHALL NOT kill a running program. A reload is an edit to
  a file, and losing an editor or a game to one would make reloading unusable.
- A plugin that is removed, turned off, or no longer asks for its pane SHALL have
  the pane released rather than left running invisibly forever.
- A program that exits on its own SHALL be reported as exited rather than drawn as
  a frozen screen, and asking again SHALL be able to start it afresh.

#### Scenario: The interface reloads

- **WHEN** the interface is reloaded while a program is running
- **THEN** the program keeps running and its pane is shown again

#### Scenario: The plugin is turned off

- **WHEN** a plugin holding a pane is disabled or removed
- **THEN** its program is not left running unreachably

#### Scenario: The program exits

- **WHEN** the program in a pane exits
- **THEN** the pane reports that rather than showing a frozen screen
- **AND** asking for it again starts it afresh

### Requirement: A plugin may not hold unbounded panes

The system SHALL bound how many programs one plugin may hold at once, and SHALL
refuse further ones rather than starting them. The refusal SHALL be visible to the
plugin, so a pane can say why it has nothing to show instead of appearing broken.

This exists because the bounds that make asking for a program's output safe do not
transfer: a pane has no timeout by design and produces output for as long as it
lives.

#### Scenario: A plugin asks for one too many

- **WHEN** a plugin asks for a program beyond the limit
- **THEN** the program is not started
- **AND** the plugin can tell that it was refused, and why

#### Scenario: A released pane frees its place

- **WHEN** a plugin releases one of its panes
- **THEN** it may start another

### Requirement: A program a plugin asked for is not a session

The system SHALL keep a plugin's program pane out of everything that enumerates
sessions. It SHALL NOT appear in the session list, SHALL NOT be counted among
sessions, and SHALL NOT be treated as an agent whose status can be reported.

This is stated because the machinery underneath is shared with sessions, and a
pane that leaked into that enumeration would be a session the user cannot delete,
restart, or explain.

#### Scenario: The session list is unaffected

- **WHEN** a plugin is holding a program pane
- **THEN** the sessions reported are exactly those that would be reported without it

#### Scenario: Status is not claimed for it

- **WHEN** a program pane is running
- **THEN** no agent status is derived from it
