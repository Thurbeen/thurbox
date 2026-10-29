import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { parse } from 'node-html-parser';

// Reads the built site, so run it after `npm run build:website`.
const built = (path) => parse(readFileSync(`_site/${path}`, 'utf8'));
const text = (node) => node.textContent.replace(/\s+/g, ' ');

test('plugin catalog lists Voice with its repository, install steps and data flow', () => {
  const repo = 'https://github.com/TMSCH/thurbox-voice';
  const card = built('plugins.html')
    .querySelectorAll('[data-plugin]')
    .find((c) => c.querySelector('h2 a')?.getAttribute('href') === repo);
  assert.ok(card, 'a catalog card links to the thurbox-voice repository');
  assert.equal(card.querySelector('h2').textContent, 'thurbox-voice');
  assert.equal(card.getAttribute('data-badge'), 'community');
  assert.ok(card.querySelector(`a[href="${repo}#readme"]`), 'setup link goes to the README');
  const install = text(parse(card.querySelector('pre').innerHTML));
  assert.match(install, /git clone https:\/\/github\.com\/TMSCH\/thurbox-voice/);
  assert.match(install, /thurbox-cli plugin install git\+https:\/\/github\.com\/TMSCH\/thurbox-voice/);
  const copy = text(card);
  assert.match(copy, /transcribed on your machine/i);
  assert.match(copy, /cleanup/i, 'the optional text cleanup pass is disclosed');
  assert.doesNotMatch(copy, /model picker|hold mode|hold to talk/i);
});

test('homepage does not promote performance', () => {
  const home = built('index.html');
  assert.ok(!home.querySelector('#performance'), 'no performance section');
  assert.ok(!home.querySelector('a[href*="performance"]'), 'no performance links');
  assert.doesNotMatch(text(home), /benchmark|five-way|frames a second|% of one core|Herdr/i);
});

test('plugin catalog lists Automations with its repository, install command and current requirements', () => {
  const repo = 'https://github.com/Thurbeen/thurbox-automations';
  const card = built('plugins.html')
    .querySelectorAll('[data-plugin]')
    .find((c) => c.querySelector('h2 a')?.getAttribute('href') === repo);
  assert.ok(card, 'a catalog card links to the thurbox-automations repository');
  assert.equal(card.getAttribute('data-badge'), 'official');
  assert.equal(
    text(parse(card.querySelector('pre').innerHTML)).trim(),
    'thurbox-cli plugin install git+https://github.com/Thurbeen/thurbox-automations',
  );
  const copy = text(card);
  assert.match(copy, /run history|recent runs/i);
  assert.match(copy, /2\.56\.5/);
  assert.match(copy, /2\.57\.0/, 'names the release that brought create and edit');
  assert.doesNotMatch(copy, /need a release with/i, 'create and edit have shipped');
  const gif = card.querySelector('img').getAttribute('src');
  assert.equal(gif, 'https://raw.githubusercontent.com/Thurbeen/thurbox-automations/main/media/demo.gif');
});

test('features page points automations at their pane', () => {
  const features = built('docs/features.html');
  const repo = 'https://github.com/Thurbeen/thurbox-automations';
  assert.ok(features.querySelector(`a[href="${repo}"]`), 'links the automations pane');
  assert.doesNotMatch(text(features), /No pane, but they still fire/);
});

const retiredFleet = [
  /sync-registry\.sh|install-extension\.sh|update-from-template\.sh/,
  /use this template|is a template/i,
  /nothing else is periodic/i,
];

// Fleet v1: the Python control plane that installed as a Thurbox extension.
const fleetV1 = [/uv run fleet/, /\/fleet-onboarding/, /Mission Control/, /Kanban/, /result\.md/, /--kind result/];

test('orchestration page leads with Fleet v2, its release installer and Fleet Lens', () => {
  const page = built('docs/orchestration.html');
  const fleet = 'https://github.com/Thurbeen/fleet';
  assert.ok(page.querySelector(`a[href="${fleet}"]`), 'links the Fleet repository');
  assert.ok(page.querySelector('a[href="https://github.com/Thurbeen/thurbox-fleetlens"]'), 'links Fleet Lens');
  const h2s = page.querySelectorAll('h2').map((h) => h.getAttribute('id'));
  assert.equal(h2s[0], 'fleet', 'Fleet is the first section');
  const copy = text(page);
  const code = page.querySelectorAll('pre').map((pre) => text(parse(pre.innerHTML))).join('\n');
  assert.match(
    code,
    /curl -fsSL https:\/\/github\.com\/Thurbeen\/fleet\/releases\/latest\/download\/install-release\.sh \| sh/,
  );
  assert.match(code, /thurbox-cli plugin install git\+https:\/\/github\.com\/Thurbeen\/thurbox-fleetlens/);
  assert.match(copy, /fleet result/, 'the worker reports its own result');
  for (const stale of [...retiredFleet, ...fleetV1]) assert.doesNotMatch(copy, stale);
  assert.doesNotMatch(page.innerHTML, /raw\.githubusercontent\.com\/Thurbeen\/fleet\//, 'no file from Fleet v1');
});

test('Fleet v1 is no longer offered as a Thurbox extension', () => {
  const extensions = built('docs/extensions.html');
  const copy = text(extensions);
  for (const stale of [...retiredFleet, ...fleetV1]) assert.doesNotMatch(copy, stale);
  assert.doesNotMatch(copy, /\bfleet\b/i, 'the manifest example and the catalog are not Fleet');
  assert.doesNotMatch(built('index.html').innerHTML, /<code>fleet<\/code>/);
});

test('plugin catalog lists Fleet Lens in place of the Fleet v1 extension', () => {
  const cards = built('plugins.html').querySelectorAll('[data-plugin]');
  const repos = cards.map((c) => c.querySelector('h2 a').getAttribute('href'));
  assert.ok(!repos.includes('https://github.com/Thurbeen/fleet'), 'Fleet v1 is not a plugin');
  const repo = 'https://github.com/Thurbeen/thurbox-fleetlens';
  const card = cards.find((c) => c.querySelector('h2 a').getAttribute('href') === repo);
  assert.ok(card, 'a catalog card links to thurbox-fleetlens');
  assert.equal(card.getAttribute('data-kind'), 'pane');
  assert.equal(card.getAttribute('data-badge'), 'official');
  assert.match(text(parse(card.querySelector('pre').innerHTML)), /thurbox-cli plugin install git\+https:\/\/github\.com\/Thurbeen\/thurbox-fleetlens/);
  assert.equal(
    card.querySelector('img').getAttribute('src'),
    'https://raw.githubusercontent.com/Thurbeen/thurbox-fleetlens/main/media/demo.gif',
  );
});

test('homepage links each official plugin from the interface section, and stays light on commands', () => {
  const home = built('index.html');
  const section = home.querySelector('#customizable');
  const plugins = ['info-panel', 'code-review', 'automations', 'auto-continue', 'fleetlens', 'doom'];
  for (const name of plugins) {
    const repo = `https://github.com/Thurbeen/thurbox-${name}`;
    const link = section.querySelector(`a[href="${repo}"]`);
    assert.ok(link, `links ${repo}`);
    assert.ok(text(link.parentNode).trim().length > `thurbox-${name}`.length, `${name} says what it does`);
  }
  assert.ok(section.querySelector('a[href="plugins.html"]'), 'points at the full catalog');
  assert.doesNotMatch(home.innerHTML, /thurbox-discord|thurbox-plugin-orchestrator|thurbox-ask/);
  // An install line is enough for a first visit; the rest lives in the docs.
  const codeBlocks = home.querySelectorAll('pre').filter((pre) => /language-/.test(pre.innerHTML));
  assert.equal(codeBlocks.length, 3, 'the install lines are the only code blocks');
  assert.doesNotMatch(text(home), /thurbox-cli (plugin|session) /);
  assert.ok(home.querySelector('a[href="docs/installation.html"]'), 'other install routes are linked');
  assert.match(text(home.querySelector('#how-it-works')), /Run thurbox\. The session/, 'no space before the period');
});
