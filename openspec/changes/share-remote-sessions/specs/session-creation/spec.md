## ADDED Requirements

### Requirement: Creation on a shareable host is performed by the host

When the named host is shareable and has a usable `thurbox-cli`, the system
SHALL have that CLI create the session — repository resolution, worktree,
extra repositories, hooks, agent launch and record — using the host's own
agent definitions and configuration, and SHALL record the session locally
under the id the host assigned. The phases exposed while this happens SHALL
include one that names the host. A parent given for such a session SHALL be a
session on the same host; any other parent SHALL be refused before any work
begins.

#### Scenario: Delegated creation

- **WHEN** a create command names a shareable host with a usable CLI
- **THEN** the session is created on the host by its CLI and appears locally
  with the host's id

#### Scenario: A parent on another host

- **WHEN** the create command names a parent that does not run on the target
  host
- **THEN** the creation is refused before any repository work

#### Scenario: A host with no usable CLI

- **WHEN** the named host has no usable CLI and none can be provisioned
- **THEN** creation proceeds as it does today, from the caller, and the
  session notes that sharing is off for the host
