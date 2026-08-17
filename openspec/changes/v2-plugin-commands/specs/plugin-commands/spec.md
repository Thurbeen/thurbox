# plugin-commands Specification

## Purpose
Defines what a plugin may ask to be run on the user's behalf, where that program
runs, what comes back, what is refused, and what the user controls — so that a
pane can be written over a third-party program's output without the interface
ever waiting on that program, and without a file dropped into the interface
directory silently gaining the power to run anything.

## ADDED Requirements

### Requirement: A plugin can ask for a program to be run and read its output

A plugin SHALL be able to ask for a program to be run and, on a later frame,
read that program's standard output, standard error and exit status. The ask
SHALL NOT block: the plugin's own frame SHALL complete whether or not the
program has finished.

#### Scenario: A program is asked for and completes

- **WHEN** a plugin asks for a program to be run
- **THEN** the frame in which it asks completes without waiting
- **AND** a later frame can read that program's output, error output and exit
  status

#### Scenario: The program has not finished yet

- **WHEN** a plugin reads a run it has asked for and the program is still running
- **THEN** the run reads as in-progress, distinguishably from one that finished
  and produced nothing

#### Scenario: A program fails

- **WHEN** a program exits non-zero, or cannot be started at all
- **THEN** the failure is readable — the exit status, or the reason it could not
  be started — rather than being reported as empty output

### Requirement: A program runs where the session it is about lives

A run SHALL be rooted in a named session's working directory. When that session
runs on a remote host, the program SHALL run on that host; when it runs locally,
the program SHALL run locally. A run SHALL NOT fall back to the local machine
when the session's host cannot be resolved.

#### Scenario: A local session

- **WHEN** a program is asked for against a local session
- **THEN** it runs on this machine, in that session's working directory

#### Scenario: A remote session

- **WHEN** a program is asked for against a session on a remote host
- **THEN** it runs on that host, in that session's working directory there

#### Scenario: The host cannot be resolved

- **WHEN** a session names a host the configuration no longer describes
- **THEN** the run fails with a reason naming the host
- **AND** the program is not run on the local machine instead

#### Scenario: The session does not exist

- **WHEN** a run names a session that is not present
- **THEN** the run fails with a reason, and nothing is executed

### Requirement: Output is bounded and a run cannot hang the interface

Captured output SHALL be capped, and every run SHALL be bounded by a wall-clock
timeout. A program that produces unbounded output, or never exits, SHALL NOT
prevent the interface from rendering, and SHALL NOT grow memory without limit.

#### Scenario: A program prints without end

- **WHEN** a program's output exceeds the cap
- **THEN** the captured output is truncated at the cap
- **AND** the run is marked as truncated, so a pane can say so rather than
  showing a silent partial answer

#### Scenario: A program never exits

- **WHEN** a program is still running after the timeout
- **THEN** it is terminated and the run reports that it timed out

#### Scenario: The interface keeps drawing

- **WHEN** any run is in flight, however slow
- **THEN** the interface continues to render and accept input at its normal rate

### Requirement: A pane may compose several programs at once

Runs SHALL be independently addressable, so one plugin can have several
programs outstanding and render each as it arrives. A run that has completed
SHALL be readable while another is still in flight.

#### Scenario: Three programs, one pane

- **WHEN** a plugin asks for three different programs
- **THEN** each is readable under its own key
- **AND** a program that finished can be drawn while the others are still running

#### Scenario: The slowest program does not hold the others

- **WHEN** one of several outstanding programs is much slower than the rest
- **THEN** the pane can draw the completed ones without waiting for it

### Requirement: Repeated asks do not repeatedly run the program

A plugin SHALL be able to ask for the same run on every frame — which is how a
pane keeps a value current — without the program being run on every frame. A run
SHALL be re-run only when its result is stale by a stated policy, or when the
plugin explicitly asks for it to be refreshed.

#### Scenario: A pane asks every frame

- **WHEN** a plugin asks for the same program on consecutive frames
- **THEN** the program is not run again while its result is fresh
- **AND** the previous result stays readable throughout

#### Scenario: The result goes stale

- **WHEN** a result is older than the staleness policy and is asked for again
- **THEN** the program is run again
- **AND** the previous result remains readable until the new one arrives

#### Scenario: A refresh is asked for explicitly

- **WHEN** a plugin asks for a run to be refreshed
- **THEN** the program is run again regardless of how fresh the previous result was

### Requirement: Concurrent runs are bounded and queued

The number of programs running at once SHALL be bounded. Asks beyond that bound
SHALL be queued and run later, not dropped and not refused.

#### Scenario: More asks than the bound

- **WHEN** a plugin asks for more programs than the concurrency bound allows
- **THEN** the excess are queued
- **AND** each eventually runs and becomes readable

#### Scenario: A stuck program does not block every other

- **WHEN** a queued run is waiting behind a program that will time out
- **THEN** it runs once that program is terminated by the timeout

### Requirement: Running programs requires a declaration and the user's trust

A plugin SHALL declare that it runs programs before it may run any, and SHALL be
granted that capability only for a plugin the user has trusted. A plugin that has
not declared it, or that the user has not trusted, SHALL be refused — visibly
rather than silently — and nothing SHALL be executed.

Trust SHALL be per plugin, not global: trusting one plugin SHALL NOT grant the
capability to another.

#### Scenario: A plugin that did not declare it

- **WHEN** a plugin that has not declared the capability asks for a program
- **THEN** the ask is refused and the refusal is reported
- **AND** nothing is executed

#### Scenario: A plugin that declared it but is not trusted

- **WHEN** a plugin that declared the capability has not been trusted
- **THEN** its asks are refused and the refusal is reported
- **AND** nothing is executed

#### Scenario: Trusting one plugin does not trust another

- **WHEN** one plugin is trusted and another, also declaring the capability, is not
- **THEN** the trusted one may run programs and the other may not

### Requirement: Trust is granted, shown and revoked where the files are listed

The surface that lists the interface's files SHALL show which of them ask to run
programs and which are trusted, and SHALL be where trust is granted and revoked.
Revoking SHALL take effect without restarting the interface.

#### Scenario: A plugin asks for the capability

- **WHEN** a plugin declaring the capability is listed
- **THEN** it is identifiable as one that runs programs, and as trusted or not,
  without reading its source

#### Scenario: Trust is granted

- **WHEN** the user trusts a listed plugin
- **THEN** that plugin may run programs from then on, including after a restart

#### Scenario: Trust is revoked

- **WHEN** the user revokes trust in a plugin
- **THEN** its subsequent asks are refused, without the interface being restarted

#### Scenario: A trusted file has changed since it was trusted

- **WHEN** a trusted plugin's contents differ from what was trusted
- **THEN** the listing says so, so the user can re-examine it
