#!/usr/bin/env node

import { readFileSync, writeFileSync } from 'node:fs';

const HALF_BLOCK = '▀';

const [, , inPath, outPath, startArg, endArg] = process.argv;
const start = Number(startArg);
const end = Number(endArg);

if (!inPath || !outPath || !Number.isFinite(start) || !Number.isFinite(end) || end <= start) {
  console.error('usage: trim-cast.mjs <in.cast> <out.cast> <start-seconds> <end-seconds>');
  process.exit(1);
}

// asciinema 3 writes v3 casts, whose events carry intervals, not the absolute times read here.
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
    continue;
  }
  const [t, kind, data] = event;
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
