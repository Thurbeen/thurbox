# Jira issue trackers

Each row's `query` is a JQL string (it may contain spaces and is used verbatim),
e.g. `project = ENG AND statusCategory != Done`. Set `push_back` to `yes` to
push thurbox status changes back to Jira (a task marked done transitions the
issue into the Done category; reopened otherwise) — leave it `no` until you
trust the sync. Note push-back is all-or-nothing across Jira tasks. Imported
tasks carry `source=jira` and `external_id="<issue key>"`. Set `JIRA_BASE_URL`,
`JIRA_EMAIL`, and `JIRA_API_TOKEN` first (id.atlassian.com → API tokens).

| name    | query                                    | push_back |
|---------|------------------------------------------|-----------|
| example | project = ENG AND statusCategory != Done | no        |
