# Linear issue trackers

Each row's `query` is a Linear team key (the prefix on issue ids, e.g. `ENG` for
`ENG-7`); all of that team's issues are synced. Set `push_back` to `yes` to push
thurbox status changes back to Linear (a task marked done moves the issue to a
completed state; reopened to an unstarted state) — leave it `no` until you trust
the sync. Imported tasks carry `source=linear` and
`external_id="<issue identifier>"`. Set the `LINEAR_API_KEY` env var first
(Settings → Security & access → Personal API keys).

| name    | query | push_back |
|---------|-------|-----------|
| example | ENG   | no        |
