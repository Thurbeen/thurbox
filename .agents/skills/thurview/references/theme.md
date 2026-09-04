# Theme

A review looks like the project it reviews. `theme.yaml` in the review
directory carries the tokens; `publish` validates it and seals it with the
revision. An empty or absent file gives the default skin.

## Decide the look, in this order

Move to the next step only when the current one yields nothing.

1. The user asked for a specific look or named a design system: use that.
2. Inspect the reviewed project at the head commit and match its own design
   system: a Tailwind or theme config, CSS custom properties or design
   tokens, a component library theme, brand assets, an existing styled page
   or website directory, font files it ships. Read the real values; do not
   guess. Record what you read in `source`.
3. Nothing found: keep the default skin. Do not invent a palette.

State which of the three you used when you hand over the review.

## theme.yaml

```yaml
name: acme-web                       # shown in the review menu
source: tailwind.config.ts, src/styles/tokens.css
mode: light                          # dark (default) or light

colors:                              # any CSS color; every key optional
  bg: "#ffffff"                      # page
  bg2: "#f6f7f9"                     # panels, bars
  bg3: "#eef0f3"                     # hover
  code: "#f8fafc"                    # code surfaces
  fg: "#111827"                      # text
  fg2: "#4b5563"                     # secondary text
  muted: "#9ca3af"
  line: "#e5e7eb"                    # borders
  accent: "#2563eb"                  # headings, primary actions, status
  link: "#2563eb"                    # links and anchor links
  ok: "#16a34a"
  warn: "#d97706"
  del: "#dc2626"
  add: "rgb(22 163 74 / .12)"        # added-line background
  remove: "rgb(220 38 38 / .12)"     # deleted-line background
  select: "rgb(217 119 6 / .18)"     # highlighted-line background

fonts:
  display: "Inter, sans-serif"       # brand, buttons, document headings
  body: "Inter, sans-serif"
  mono: "'JetBrains Mono', monospace"
  stylesheets:                       # external font css, loaded by the page
    - https://fonts.googleapis.com/css2?family=Inter:wght@400;700&display=swap
  files:                             # font files the project ships, read at head
    - { family: Acme Sans, path: web/public/fonts/acme-sans.woff2, weight: "400 700" }

shape:
  radius: 8px                        # 0 in the default skin
  bevel: false                       # pixel bevels on panels and buttons
  glow: false                        # neon text glow on headings and tabs
  scanlines: false                   # CRT overlay
  headingTransform: none             # uppercase in the default skin

code:                                # syntax palette; any key optional
  keyword: "#7c3aed"
  string: "#15803d"
  function: "#b45309"
  type: "#b45309"
  variable: "#0369a1"
  number: "#b45309"
  comment: "#9ca3af"
  punctuation: "#6b7280"
  operator: "#374151"
  tag: "#be185d"
  fg: "#111827"

css: |                               # optional extra rules, appended last
  .doc h1 { letter-spacing: 0; }
```

Rules:

- Keep contrast readable: body text against `bg`, code against `code`.
- A light project gets `mode: light` and light backgrounds; the default skin
  is dark and its bevels, glow and scanlines are off by default only when you
  set them so.
- `fonts.files` paths must exist at the pinned head commit; `publish`
  rejects a missing one. Font stylesheets are fetched by the reader's
  browser.
- Map the project's semantic roles, not its whole palette: primary to
  `accent` and `link`, success/warning/danger to `ok`/`warn`/`del`.
- Unknown keys fail validation.
