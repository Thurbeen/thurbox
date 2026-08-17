## 1. The frame

- [ ] 1.1 A title of styled runs, replacing the plain string
- [ ] 1.2 Title alignment, left or right
- [ ] 1.3 Border overlays: an edge, an anchor, and runs
- [ ] 1.4 Clip an overlay from its anchored end; never overwrite a corner
- [ ] 1.5 Draw border, then overlays, then content

## 2. The library

- [ ] 2.1 One `lib/chrome.lua` holding the measuring and clipping six plugins
      copy today
- [ ] 2.2 Delete the copies from every plugin that has one

## 3. The panes

- [ ] 3.1 Session list composes a frame instead of painting cells
- [ ] 3.2 File viewer
- [ ] 3.3 Tasks pane and task detail
- [ ] 3.4 Automations pane and automation detail
- [ ] 3.5 Agent pane, including the tab strip and chevron as overlays

## 4. The float probe

- [ ] 4.1 A closed modal is not rendered

## 5. Proof

- [ ] 5.1 Every converted pane is cell-identical to its recording
- [ ] 5.2 A 30-row pane renders in materially less time than before, measured
- [ ] 5.3 No plugin defines `new_cells`, `place_text` or `cells_to_spans`
- [ ] 5.4 A plugin gets v1-looking chrome without a cell buffer, shown by a
      plugin that does so in under twenty lines
