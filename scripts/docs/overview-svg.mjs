import { writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const SANS = "-apple-system,BlinkMacSystemFont,'Segoe UI','Noto Sans',Helvetica,Arial,sans-serif";
const MONO = "ui-monospace,SFMono-Regular,'SF Mono',Menlo,Consolas,'Liberation Mono',monospace";

const PALETTES = {
  light: {
    fg: '#1f2328',
    dim: '#59636e',
    line: '#c8d1da',
    card: '#ffffff',
    sunk: '#f6f8fa',
    accent: '#c62f28',
    accentSoft: '#fff3f2',
    ok: '#1a7f37',
  },
  dark: {
    fg: '#e6edf3',
    dim: '#9198a1',
    line: '#3d444d',
    card: '#151b23',
    sunk: '#1c232c',
    accent: '#ff5c54',
    accentSoft: '#2a1517',
    ok: '#6eff6e',
  },
};

const TITLE = 'thurbox: one interface, several Fleets';
const DESC =
  'thurbox lists the sessions of every Fleet in settings.toml; Ctrl+N picks a Fleet, a place and a ' +
  'repository. Fleet laptop runs on this machine, and Fleet box on another machine, reached over ssh ' +
  'or WSL; on each, a systemd unit runs fleet up, never thurbox. Each Fleet starts a ' +
  'runtime (runtime-mux, or yours), which runs a workspace provider (workspace-git, or yours) and ' +
  'runs sessions at a place: Fleet laptop at local (the laptop) and mux/devbox over ssh, Fleet box ' +
  'at local (box itself) and mux/wsl-dev. A session is a Task of one Fleet: it keeps running when ' +
  'thurbox quits, and a Fleet that cannot be reached keeps its rows, marked unreachable.';

const WIDTH = 900;
const HEIGHT = 452;
const THURBOX = [20, 180];
const FLEET = [244, 404];
const RUNTIME = [452, 668];
const PLACE = [714, 880];
const LANE_TOPS = [76, 226];
const LANE_HEIGHT = 130;
const PLACE_HEIGHT = 58;
const TOP = LANE_TOPS[0];
const BOTTOM = LANE_TOPS[1] + LANE_HEIGHT;

const LANES = [
  {
    fleet: 'Fleet laptop',
    where: 'on this machine',
    how: 'a systemd unit runs',
    link: 'fleet',
    places: [
      ['local', 'the laptop'],
      ['mux/devbox', 'devbox, over ssh'],
    ],
  },
  {
    fleet: 'Fleet box',
    where: 'on another machine',
    how: 'a systemd unit runs',
    link: 'ssh · WSL',
    places: [
      ['local', 'box itself'],
      ['mux/wsl-dev', 'WSL on box'],
    ],
  },
];

const escape = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

const text = (x, y, content, cls, anchor) =>
  `<text x="${x}" y="${y}" class="${cls}"${anchor ? ` text-anchor="${anchor}"` : ''}>${content}</text>`;

const rect = (x, y, width, height, cls, radius) =>
  `<rect x="${x}" y="${y}" width="${width}" height="${height}" rx="${radius}" class="${cls}"/>`;

const circle = (x, y, radius, cls) => `<circle cx="${x}" cy="${y}" r="${radius}" class="${cls}"/>`;

const orYours = (name) => `${name}<tspan class="or"> · or yours</tspan>`;

const pill = (centre, y, label) => {
  const width = label.length * 6 + 16;
  return [
    rect(centre - width / 2, y - 9, width, 18, 'pill', 9),
    text(centre, y + 3.5, escape(label), 'el', 'middle'),
  ];
};

const style = (c) =>
  [
    `text{font-family:${SANS};fill:${c.fg};font-size:12.5px}`,
    '.h{font-size:20px;font-weight:600}',
    `.col{font-size:11.5px;fill:${c.dim}}`,
    '.n{font-size:14px;font-weight:600}',
    `.na{font-size:15px;font-weight:700;fill:${c.accent}}`,
    `.d{fill:${c.dim};font-size:12px}`,
    `.m{font-family:${MONO};font-size:12.5px}`,
    `.s{font-family:${MONO};font-size:11px}`,
    `.or{fill:${c.dim};font-size:11px;font-weight:400}`,
    `.r{font-size:11px;fill:${c.dim}}`,
    `.el{font-size:10.5px;fill:${c.dim}}`,
    `.b{fill:${c.card};stroke:${c.line};stroke-width:1}`,
    `.ba{fill:${c.accentSoft};stroke:${c.accent};stroke-width:1.5}`,
    `.bs{fill:${c.sunk};stroke:${c.line};stroke-width:1}`,
    `.pill{fill:${c.card};stroke:${c.line};stroke-width:1}`,
    `.w{fill:none;stroke:${c.line};stroke-width:1.5}`,
    `.dot{fill:${c.accent}}`,
    `.ok{fill:${c.ok}}`,
    `.ft{fill:${c.sunk}}`,
    `.fb{fill:${c.accent}}`,
    '.fs{font-weight:600}',
  ].join('');

const thurboxCard = () => {
  const x = THURBOX[0] + 16;
  return [
    rect(THURBOX[0], TOP, THURBOX[1] - THURBOX[0], BOTTOM - TOP, 'ba', 12),
    text(x, TOP + 30, 'thurbox', 'na'),
    text(x, TOP + 58, 'One list of sessions', ''),
    text(x, TOP + 76, 'from every Fleet in', ''),
    text(x, TOP + 94, 'settings.toml', 'm'),
    rect(x, BOTTOM - 82, 58, 22, 'bs', 5),
    text(x + 29, BOTTOM - 67, 'Ctrl+N', 's', 'middle'),
    text(x, BOTTOM - 36, 'picks a Fleet, a place', 'd'),
    text(x, BOTTOM - 18, 'and a repository', 'd'),
  ];
};

const lane = (spec, top) => {
  const middle = top + LANE_HEIGHT / 2;
  const places = [top, top + LANE_HEIGHT - PLACE_HEIGHT];
  const split = (RUNTIME[1] + PLACE[0]) / 2;
  const branches = places.map((y) => `M${split} ${middle}V${y + PLACE_HEIGHT / 2}H${PLACE[0]}`).join('');
  const fleetX = FLEET[0] + 16;
  const runtimeX = RUNTIME[0] + 16;
  return [
    `<path d="M${THURBOX[1]} ${middle}H${FLEET[0]}M${FLEET[1]} ${middle}H${RUNTIME[0]}" class="w"/>`,
    `<path d="M${RUNTIME[1]} ${middle}H${split}${branches}" class="w"/>`,
    ...pill((THURBOX[1] + FLEET[0]) / 2, middle, spec.link),
    ...pill((FLEET[1] + RUNTIME[0]) / 2, middle, 'start'),
    ...pill(split - 1, middle, 'runs'),
    rect(FLEET[0], top, FLEET[1] - FLEET[0], LANE_HEIGHT, 'b', 12),
    circle(fleetX + 4, top + 26, 4, 'dot'),
    text(fleetX + 14, top + 30, spec.fleet, 'n'),
    text(fleetX, top + 58, spec.where, ''),
    text(fleetX, top + 84, spec.how, 'd'),
    text(fleetX, top + 102, 'fleet up', 'd m'),
    rect(RUNTIME[0], top, RUNTIME[1] - RUNTIME[0], LANE_HEIGHT, 'b', 12),
    text(runtimeX, top + 22, 'runtime', 'r'),
    text(runtimeX, top + 42, orYours('runtime-mux'), 'n m'),
    rect(RUNTIME[0] + 10, top + 58, RUNTIME[1] - RUNTIME[0] - 20, 62, 'bs', 8),
    text(runtimeX, top + 80, 'workspace provider', 'r'),
    text(runtimeX, top + 100, orYours('workspace-git'), 'n m'),
    ...spec.places.flatMap(([name, detail], i) => [
      rect(PLACE[0], places[i], PLACE[1] - PLACE[0], PLACE_HEIGHT, 'b', 10),
      text(PLACE[0] + 14, places[i] + 24, name, 'n m'),
      text(PLACE[0] + 14, places[i] + 43, detail, 'd'),
      circle(PLACE[1] - 16, places[i] + 20, 3.5, 'ok'),
    ]),
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
      'A session is a Task of one Fleet.<tspan class="d" font-weight="400"> It keeps running when thurbox quits.</tspan>',
      'fs',
    ),
    text(38, y + 40, 'A Fleet that cannot be reached keeps its rows, marked unreachable.', 'd'),
  ];
};

export const overviewSvg = (theme) => {
  const body = [
    `<title id="t">${TITLE}</title>`,
    `<desc id="d">${DESC}</desc>`,
    `<style>${style(PALETTES[theme])}</style>`,
    text(20, 36, 'One interface, several Fleets', 'h'),
    ...[
      [THURBOX[0], 'interface'],
      [FLEET[0], 'engine'],
      [RUNTIME[0], 'runtime'],
      [PLACE[0], 'place'],
    ].map(([x, label]) => text(x + 2, 62, label, 'col')),
    ...thurboxCard(),
    ...LANES.flatMap((spec, i) => lane(spec, LANE_TOPS[i])),
    ...footer(),
  ];
  return [
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${WIDTH} ${HEIGHT}" width="${WIDTH}" height="${HEIGHT}" role="img" aria-labelledby="t d">`,
    ...body.map((line) => `  ${line}`),
    '</svg>',
    '',
  ].join('\n');
};

export const overviewPath = (theme) => `website/assets/overview-${theme}.svg`;

export const THEMES = Object.keys(PALETTES);

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  for (const theme of THEMES) writeFileSync(overviewPath(theme), overviewSvg(theme));
}
