---
name: thurbox-session
description: Use when explicitly asked to inspect or drive one thurbox session.
---

# Inspect one thurbox session

Check `thurbox-cli --help` and the relevant subcommand's help against the
installed binary before constructing a command. Read-only inspection comes
before effects. Use session identity and host explicitly; an unreachable
host is Unknown, not proof that a session is gone or idle.

Only send text when the requester has authorized that recipient and message.
Do not type into a busy worker, delete another session, or create a duplicate
session to work around an unreadable state. Fleet-owned work belongs to its
Task and controller; inspect it through `fleet get` and `fleet status`.
Starting a discussion session with no deliverable is a separate authorized
operation. Keep credentials off arguments and preserve source on cleanup.
