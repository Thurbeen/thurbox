# Software map

`map.yaml` describes the repository structure at head, and optionally at
base, as nested nodes. The Map tab lets the reader drill from systems to code
and shows what the change added, removed or touched.

```yaml
nodes:
  - { id: app, kind: system, label: Review app, description: Local server and UI }
  - { id: app.cli, kind: container, label: CLI, files: ["src/cli.ts"] }
  - { id: app.server, kind: container, label: Server, files: ["src/server/**"] }
  - { id: app.server.threads, kind: component, label: Threads, files: ["src/threads.ts"], anchor: createThread }
  - { id: reader, kind: person, label: Reader }
edges:
  - { from: reader, to: app.server, label: reviews in the browser }
  - { from: app.cli, to: app.server, label: publishes revisions }
base:                      # optional: structure at the base commit
  nodes: [...]
  edges: [...]
```

Rules:

- `id` is a dot path. Every parent must exist as a node (`app` before
  `app.cli`). Identity is the id; keep ids stable between base and head.
- `kind`: `person`, `system`, `container`, `component`, `code`.
- `files`: globs relative to the repository root (`*`, `**`, `?`). They link
  the node to changed files in the Files tab. A glob matching nothing at the
  pinned commit is a warning.
- `anchor`: an anchor id from `data.yaml` that opens representative code.
- Edges reference node ids. Labels are short verbs.

Model the important people, systems, containers, components and code
elements. Do not model incidental implementation detail. For a large
repository, keep the top level small and put detail one level down.

Work base first, then apply only the structural changes of the diff to get
head. Without `base`, nodes touched by the diff show as changed and nothing
shows as added or removed.

`thurview publish` validates the map with the document; map errors block
publication like document errors. An empty `nodes: []` means no map.
