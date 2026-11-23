import { writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { PALETTES, THEMES, escape, pill, rect, style, text } from './overview-svg.mjs';

const TITLE = 'Fleet: resources in a store, controllers that reconcile them';
const DESC =
  'You declare resources with fleet apply, thurbox or a script: a Task with its spec, and TaskConfig, ' +
  'EnvProvider, ChangeRequest, CronTask, Board, Conversation and Listener. The store keeps them. ' +
  'Controllers, run by fleet up, watch the spec, observe the runtime and the forge, compare, and act: ' +
  'the runtime makes the workspace and starts, briefs, pauses, revives or releases the session at a ' +
  'place; the forge is mirrored into ChangeRequests. The controllers write the phase and conditions ' +
  'back to the status. Each effect is recorded as an intent before it runs, and an unknown ' +
  'observation never means gone.';

const WIDTH = 900;
const HEIGHT = 438;
const TOP = 76;
const BOTTOM = 340;
const YOU = [20, 136];
const STORE = [192, 432];
const CONTROLLERS = [488, 660];
const RIGHT = [716, 880];
const RUNTIME_BOTTOM = 220;
const FORGE_TOP = 240;

const LOOP_CENTRE = [(CONTROLLERS[0] + CONTROLLERS[1]) / 2, 218];
const LOOP_RADIUS = 56;
const LOOP_STEPS = [
  ['observe', -90],
  ['compare', 30],
  ['act', 150],
];
const LOOP_GAP = 30;

const KINDS = ['TaskConfig', 'EnvProvider', 'ChangeRequest', 'CronTask'];

const arrowStyle = (c) => `.ah{fill:${c.line}}.lp{fill:none;stroke:${c.accent};stroke-width:1.5}.lh{fill:${c.accent}}`;

const defs = () =>
  '<defs>' +
  '<marker id="ah" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="8" markerHeight="8" orient="auto-start-reverse"><path d="M0 0L8 4L0 8z" class="ah"/></marker>' +
  '<marker id="lh" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto"><path d="M0 0L8 4L0 8z" class="lh"/></marker>' +
  '</defs>';

const arrow = (x1, y, x2, label, both = false) => [
  `<path d="M${x1} ${y}H${x2}" class="w" marker-end="url(#ah)"${both ? ' marker-start="url(#ah)"' : ''}/>`,
  text((x1 + x2) / 2, y - 7, escape(label), 'el', 'middle'),
];

const point = (degrees) => {
  const radians = (degrees * Math.PI) / 180;
  return [LOOP_CENTRE[0] + LOOP_RADIUS * Math.cos(radians), LOOP_CENTRE[1] + LOOP_RADIUS * Math.sin(radians)].map(
    (v) => Math.round(v * 10) / 10,
  );
};

const loop = () =>
  LOOP_STEPS.flatMap(([label, degrees], i) => {
    const next = LOOP_STEPS[(i + 1) % LOOP_STEPS.length][1] + (i === LOOP_STEPS.length - 1 ? 360 : 0);
    const [fromX, fromY] = point(degrees + LOOP_GAP);
    const [toX, toY] = point(next - LOOP_GAP);
    const [x, y] = point(degrees);
    return [
      `<path d="M${fromX} ${fromY}A${LOOP_RADIUS} ${LOOP_RADIUS} 0 0 1 ${toX} ${toY}" class="lp" marker-end="url(#lh)"/>`,
      ...pill(x, y, label),
    ];
  });

const youCard = () => {
  const x = YOU[0] + 14;
  return [
    rect(YOU[0], TOP, YOU[1] - YOU[0], BOTTOM - TOP, 'b', 12),
    text(x, TOP + 30, 'declare', 'n'),
    text(x, TOP + 54, 'what you want', 'd'),
    ...['fleet apply', 'thurbox', 'a script'].flatMap((label, i) => [
      rect(x, TOP + 140 + i * 34, YOU[1] - YOU[0] - 28, 24, 'bs', 6),
      text(x + 10, TOP + 156 + i * 34, label, 's'),
    ]),
  ];
};

const storeCard = () => {
  const x = STORE[0] + 14;
  const inner = STORE[1] - STORE[0] - 28;
  return [
    rect(STORE[0], TOP, STORE[1] - STORE[0], BOTTOM - TOP, 'b', 12),
    text(x, TOP + 30, 'resources', 'n'),
    rect(x, TOP + 56, inner, 84, 'bs', 8),
    text(x + 12, TOP + 76, 'Task', 'n m'),
    text(x + 12, TOP + 98, 'spec', 'r'),
    text(x + 52, TOP + 98, 'harness · place · brief', 'd'),
    text(x + 12, TOP + 120, 'status', 'r'),
    text(x + 52, TOP + 120, 'phase · conditions', 'd'),
    ...KINDS.flatMap((kind, i) => {
      const y = TOP + 152 + Math.floor(i / 2) * 32;
      const cx = x + (i % 2) * (inner / 2 + 4);
      return [rect(cx, y, inner / 2 - 4, 24, 'bs', 6), text(cx + 8, y + 16, kind, 's')];
    }),
    text(x, TOP + 238, 'Board · Conversation · …', 'd'),
  ];
};

const controllersCard = () => {
  const x = CONTROLLERS[0] + 14;
  return [
    rect(CONTROLLERS[0], TOP, CONTROLLERS[1] - CONTROLLERS[0], BOTTOM - TOP, 'ba', 12),
    text(x, TOP + 30, 'controllers', 'na'),
    text(x, TOP + 52, 'run by', 'd'),
    text(x + 42, TOP + 52, 'fleet up', 'd m'),
    ...loop(),
    text((CONTROLLERS[0] + CONTROLLERS[1]) / 2, BOTTOM - 18, 'one reconcile per sweep', 'd', 'middle'),
  ];
};

const runtimeCard = () => {
  const x = RIGHT[0] + 14;
  return [
    rect(RIGHT[0], TOP, RIGHT[1] - RIGHT[0], RUNTIME_BOTTOM - TOP, 'b', 12),
    text(x, TOP + 22, 'runtime', 'r'),
    text(x, TOP + 42, 'runtime-mux', 'n m'),
    rect(RIGHT[0] + 10, TOP + 58, RIGHT[1] - RIGHT[0] - 20, 74, 'bs', 8),
    text(x, TOP + 80, 'session', 'n'),
    text(x, TOP + 100, 'the agent, at a place', 'd'),
    text(x, TOP + 120, 'workspace', 'n'),
  ];
};

const forgeCard = () => {
  const x = RIGHT[0] + 14;
  return [
    rect(RIGHT[0], FORGE_TOP, RIGHT[1] - RIGHT[0], BOTTOM - FORGE_TOP, 'b', 12),
    text(x, FORGE_TOP + 22, 'forge', 'r'),
    text(x, FORGE_TOP + 42, 'pull requests', 'n'),
    text(x, FORGE_TOP + 66, 'GitHub, GitLab, …', 'd'),
    text(x, FORGE_TOP + 84, 'checks and reviews', 'd'),
  ];
};

const footer = () => {
  const y = BOTTOM + 22;
  return [
    rect(20, y, 860, 52, 'ft', 10),
    rect(20, y, 4, 52, 'fb', 2),
    text(
      38,
      y + 21,
      'The plan lives in the store.<tspan class="d" font-weight="400" dx="5">Sessions come and go; the spec says what they should be.</tspan>',
      'fs',
    ),
    text(38, y + 40, 'Each effect is recorded as an intent before it runs, and an unknown answer never means gone.', 'd'),
  ];
};

export const fleetLoopSvg = (theme) => {
  const palette = PALETTES[theme];
  const runtimeMiddle = (TOP + RUNTIME_BOTTOM) / 2;
  const forgeMiddle = (FORGE_TOP + BOTTOM) / 2;
  const body = [
    `<title id="t">${escape(TITLE)}</title>`,
    `<desc id="d">${escape(DESC)}</desc>`,
    `<style>${style(palette)}${arrowStyle(palette)}</style>`,
    defs(),
    text(20, 36, 'Fleet reconciles, like Kubernetes', 'h'),
    ...[
      [YOU[0], 'you'],
      [STORE[0], 'store'],
      [CONTROLLERS[0], 'engine'],
      [RIGHT[0], 'the world'],
    ].map(([x, label]) => text(x + 2, 62, label, 'col')),
    ...arrow(YOU[1], 141, STORE[0], 'apply'),
    ...arrow(STORE[1], 128, CONTROLLERS[0], 'spec'),
    ...arrow(CONTROLLERS[0], 300, STORE[1], 'status'),
    ...arrow(CONTROLLERS[1], runtimeMiddle - 22, RIGHT[0], 'effects'),
    ...arrow(RIGHT[0], runtimeMiddle + 22, CONTROLLERS[1], 'observe'),
    ...arrow(CONTROLLERS[1], forgeMiddle, RIGHT[0], 'mirror', true),
    ...youCard(),
    ...storeCard(),
    ...controllersCard(),
    ...runtimeCard(),
    ...forgeCard(),
    ...footer(),
  ];
  return [
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${WIDTH} ${HEIGHT}" width="${WIDTH}" height="${HEIGHT}" role="img" aria-labelledby="t d">`,
    ...body.map((line) => `  ${line}`),
    '</svg>',
    '',
  ].join('\n');
};

export const fleetLoopPath = (theme) => `website/assets/fleet-loop-${theme}.svg`;

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  for (const theme of THEMES) writeFileSync(fleetLoopPath(theme), fleetLoopSvg(theme));
}
