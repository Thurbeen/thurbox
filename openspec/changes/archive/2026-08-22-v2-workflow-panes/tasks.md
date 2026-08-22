## 1. Reads

- [x] 1.1 Publish tasks with title, description, status and origin
- [x] 1.2 Publish automations with name, schedule, action, enabled state and last outcome
- [x] 1.3 Publish an automation's recent runs with time, outcome and detail

## 2. Commands

- [x] 2.1 Create, retitle, re-describe and delete a task
- [x] 2.2 Cycle a task's status, persisting it
- [x] 2.3 Dispatch a task to a running session, using the prompt the CLI already builds
- [x] 2.4 Dispatch a task to a new session, delivering its context once ready
- [x] 2.5 Enable, disable, run-now and delete an automation

## 3. Plugins

- [x] 3.1 A task pane: list, status glyphs, selection, and the description in a detail view
- [x] 3.2 A dispatch picker choosing between a running session and a new one
- [x] 3.3 An automation pane: list, enabled state, last outcome
- [x] 3.4 An automation's run history, readable from the pane
- [x] 3.5 Declare every key through the registry so both panes appear in help

## 4. Proof

- [x] 4.1 A task's status round-trips through a command
- [x] 4.2 Dispatching a task advances it out of not-started
- [x] 4.3 An agent receives the task's id, title and description — not just the title
- [x] 4.4 Disabling an automation persists
