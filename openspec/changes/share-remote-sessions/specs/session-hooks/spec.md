## ADDED Requirements

### Requirement: A delegated operation fires hooks on both sides

When a session operation is performed on a shareable host by the host's CLI,
the caller's own hooks SHALL fire locally around the delegated call as they do
for any remote session today (`THURBOX_HOST` set), and the host's own hooks
SHALL fire on the host as for a session the host created itself. A refusal by
the caller's pre-hook SHALL prevent the delegation; a refusal by the host's
pre-hook SHALL be reported to the caller as the host's failure.

#### Scenario: Both hook files are present

- **WHEN** the caller's `hooks.toml` and H's `hooks.toml` each declare
  `session.post_create`, and a session is created on H from the caller
- **THEN** the caller's command runs on the caller's machine with
  `THURBOX_HOST=H`, and H's command runs on H with no `THURBOX_HOST`

#### Scenario: The host vetoes

- **WHEN** H's `session.pre_create` hook exits non-zero
- **THEN** no session is created, and the caller sees H's hook failure as the
  creation error
