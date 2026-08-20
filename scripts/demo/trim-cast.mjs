#!/usr/bin/env node
// Trim an asciicast v2 to [start,end] seconds, rebasing timestamps to 0.
//
// v2 specifically: asciinema 3 writes v3, whose lines carry *intervals* rather
// than the absolute timestamps this reads, so a cast from it would be trimmed to
// nonsense rather than refused. record-doom.sh pins asciinema 2.x for that reason.
//
// Used by record-doom.sh to cut the Doom window out of a recording that also
// contains thurbox booting and Doom's own title screen and first attract demo.
// Everything before `start` is collapsed into one instant chunk at t=0, so frame
// 1 already shows the fully painted screen instead of replaying the boot.

import { readFileSync, writeFileSync } from 'node:fs';

// The upper-half-block glyph the Doom engine paints every frame with: two
// vertical pixels per cell, top in the foreground and bottom in the background.
// It first appears the moment the program pane comes up, which is `f7` — so the
// time reported below is the origin record-doom.sh's phase arithmetic counts
// from, and how you retime START after a change.
const HALF_BLOCK = '▀';

const [, , inPath, outPath, startArg, endArg] = process.argv;
const start = Number(startArg);
const end = Number(endArg);

if (!inPath || !outPath || !Number.isFinite(start) || !Number.isFinite(end) || end <= start) {
  console.error('usage: trim-cast.mjs <in.cast> <out.cast> <start-seconds> <end-seconds>');
  process.exit(1);
}

const lines = readFileSync(inPath, 'utf8').split('\n').filter(Boolean);
const [header, ...events] = lines;
const out = [header];

let preamble = '';
const kept = [];
let firstHalfBlockAt = null;

for (const line of events) {
  let event;
  try {
    event = JSON.parse(line);
  } catch {
    // A cast killed mid-write can end in a partial line; skip it rather than die.
    continue;
  }
  const [t, kind, data] = event;
  // Only output events matter: agg replays stdout, never the recorded input.
  if (kind !== 'o') continue;
  if (firstHalfBlockAt === null && data.includes(HALF_BLOCK)) firstHalfBlockAt = t;
  if (t < start) preamble += data;
  else if (t <= end) kept.push([Number((t - start).toFixed(6)), 'o', data]);
}

out.push(JSON.stringify([0, 'o', preamble]));
for (const event of kept) out.push(JSON.stringify(event));
writeFileSync(outPath, out.join('\n') + '\n');

const firstFrame =
  firstHalfBlockAt === null ? 'none found' : `t=${firstHalfBlockAt.toFixed(2)}s`;
console.error(
  `first half-block frame: ${firstFrame}; kept ${kept.length} events ` +
    `for ${start}-${end}s -> ${outPath}`
);
