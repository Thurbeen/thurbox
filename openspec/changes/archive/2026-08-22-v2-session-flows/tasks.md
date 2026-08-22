## 1. Commands

- [x] 1.1 Add a `create` command carrying repo, optional branch, agent and host, accepted without blocking
- [x] 1.2 Run creation through `session_ops::spawn::spawn_session_headless`, unchanged
- [x] 1.3 Publish creation phases as the pipeline passes them, not a generic spinner
- [x] 1.4 Add a `fork` command that records the source session as the new session's parent
- [x] 1.5 Add a `sync` command that refuses, with a reason, when the worktree has changes that would be lost
- [x] 1.6 Add a `restore` command, and refuse a force-deleted session unless best-effort is explicit
- [x] 1.7 Report every creation failure through the in-flight channel, leaving no half-created session

## 2. Reads

- [x] 2.1 Publish the repositories a session can be created against
- [x] 2.2 Publish the available agents, from the same registry the launcher uses
- [x] 2.3 Publish the configured and discovered hosts
- [x] 2.4 Publish deleted sessions, marking those whose worktree was removed
- [x] 2.5 Publish each in-flight creation's repository and branch, so a placeholder can be grouped

## 3. Plugins

- [x] 3.1 A creation flow as a floating plugin: repository, branch, agent, host
- [x] 3.2 Offer the local host without a choice when none are configured
- [x] 3.3 Render a placeholder row in the session list, inside the repo group the session will land in
- [x] 3.4 A restore surface listing deleted sessions, marking the partially-recoverable ones
- [x] 3.5 Declare every key through the registry, so the flow appears in help
- [x] 3.6 Confirm before a sync that would discard work, reusing the confirm plugin

## 4. Proof

- [x] 4.1 Creating a session end to end against a real repository
- [x] 4.2 A failing creation surfaces its phase and leaves nothing behind
- [x] 4.3 A fork records its parent
- [x] 4.4 The flow plugin offers every choice the kernel exposes
