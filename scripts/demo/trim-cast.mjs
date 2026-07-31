#!/usr/bin/env node
// Trim an asciicast v2 to [start,end] seconds, rebasing timestamps to 0.
//
// Used by record-doom.sh to cut the Doom window out of a recording that also
// contains ~34 s of thurbox and pi booting. Everything before `start` is
// collapsed into one instant chunk at t=0, so frame 1 already shows the fully
// painted screen instead of replaying the boot.

import { readFileSync, writeFileSync } from 'node:fs';

// The upper-half-block glyph pi-doom paints every frame with. Reporting when it
// first appears is how you retime START in record-doom.sh after a change.
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
