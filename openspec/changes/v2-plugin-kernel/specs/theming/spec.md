## Purpose

Defines how the active colour scheme is chosen, resolved and delivered to
plugins, so that every pane shares one palette, a user's existing themes and
choice carry over from v1, and no plugin can hardcode a colour.

## ADDED Requirements

### Requirement: A plugin names roles, never colours

The system SHALL deliver the active theme to plugins as a set of named roles.
A plugin SHALL be able to obtain a concrete colour only by naming a role.

The role set SHALL cover every distinction the shipped interface draws,
including accent, muted and primary text, focused and unfocused borders, and a
distinct colour per session status.

#### Scenario: A plugin styles a row

- **WHEN** a plugin renders text in the accent role
- **THEN** the colour it receives is the active theme's accent

#### Scenario: A role that does not exist

- **WHEN** a plugin names a role the theme does not define
- **THEN** it receives no colour rather than an arbitrary one, and rendering continues

### Requirement: Every theme that worked in v1 still works

The system SHALL offer the same built-in themes as the previous version, each
keeping its stable identifier and its human-readable name, so a persisted
choice and any written reference remain valid.

#### Scenario: A built-in theme is selected

- **WHEN** any built-in theme is made active
- **THEN** its palette resolves and every pane renders in it

#### Scenario: Identifiers are unchanged

- **WHEN** a theme identifier that was valid previously is resolved
- **THEN** it names the same theme

### Requirement: User-defined themes are honoured

The system SHALL load user-defined themes from the user's theme configuration,
where each may name a built-in theme as its base and override individual roles.
A user theme SHALL be selectable exactly as a built-in one is.

A malformed user theme SHALL be reported and skipped, and SHALL NOT prevent the
remaining themes — or the interface — from loading.

#### Scenario: A user theme overrides one role

- **WHEN** a user theme names a base and overrides a single role
- **THEN** that role takes the override and every other role comes from the base

#### Scenario: A user theme is malformed

- **WHEN** a user theme cannot be parsed
- **THEN** it is reported, skipped, and the interface still renders in a valid theme

### Requirement: The active choice persists and is shared

The active theme SHALL be read from, and written to, the same persisted setting
the previous version used, so the choice survives a restart and is seen by other
running instances.

When no choice is recorded, or the recorded one no longer exists, the system
SHALL fall back to the default theme rather than failing to render.

#### Scenario: The choice survives a restart

- **WHEN** a theme is made active and the application is restarted
- **THEN** the same theme is active

#### Scenario: The recorded theme no longer exists

- **WHEN** the persisted theme names something that cannot be resolved
- **THEN** the default theme is used and the interface renders

### Requirement: Changing the theme restyles every pane at once

Changing the active theme SHALL change the appearance of every plugin without
any plugin being modified or reloaded, including plugins the theme's author
never saw.

#### Scenario: The theme changes while running

- **WHEN** the active theme changes
- **THEN** every pane repaints in the new palette on the next frame

#### Scenario: A third-party plugin follows the theme

- **WHEN** a plugin written by someone else renders using roles
- **THEN** it changes appearance with the theme, unedited

### Requirement: The theme list is readable by plugins

Every selectable theme — its identifier, display name, and whether it is light
or dark — SHALL be readable by a plugin, so that the surface for choosing one is
itself a plugin rather than kernel UI.

#### Scenario: A picker is a plugin

- **WHEN** a plugin renders the list of available themes
- **THEN** it can read every built-in and user theme, and which one is active

#### Scenario: Selecting a theme

- **WHEN** a plugin asks for a theme to become active
- **THEN** the choice is persisted and takes effect without a restart
