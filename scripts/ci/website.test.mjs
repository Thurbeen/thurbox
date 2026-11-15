import assert from 'node:assert/strict';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
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

test('the site names no thurbox automations', () => {
  for (const path of pages()) {
    const page = built(path);
    assert.doesNotMatch(text(page), /automations?\.json|thurbox-cli automation|thurbox-automations/i, `${path} names automations`);
    assert.ok(!page.querySelector('a[href*="thurbox-automations"]'), `${path} links the automations pane`);
  }
});

const retiredFleet = [
  /sync-registry\.sh|install-extension\.sh|update-from-template\.sh/,
  /use this template|is a template/i,
  /nothing else is periodic/i,
];

// Fleet v1: the Python control plane that installed as a Thurbox extension.
const fleetV1 = [/uv run fleet/, /\/fleet-onboarding/, /Mission Control/, /Kanban/, /result\.md/, /--kind result/];

test('orchestration page leads with Fleet, its docs in this repository and Fleet Lens', () => {
  const page = built('docs/orchestration.html');
  const docs = 'https://github.com/Thurbeen/thurbox/tree/main/docs/fleet';
  assert.ok(page.querySelector(`a[href="${docs}"]`), "links Fleet's docs in this repository");
  assert.ok(page.querySelector('a[href="https://github.com/Thurbeen/thurbox-fleetlens"]'), 'links Fleet Lens');
  const h2s = page.querySelectorAll('h2').map((h) => h.getAttribute('id'));
  assert.equal(h2s[0], 'fleet', 'Fleet is the first section');
  const copy = text(page);
  const code = page.querySelectorAll('pre').map((pre) => text(parse(pre.innerHTML))).join('\n');
  assert.match(code, /curl -fsSL https:\/\/raw\.githubusercontent\.com\/Thurbeen\/thurbox\/main\/scripts\/install\.sh \| sh/);
  assert.match(code, /thurbox-cli plugin install git\+https:\/\/github\.com\/Thurbeen\/thurbox-fleetlens/);
  assert.match(copy, /fleet result/, 'the worker reports its own result');
  for (const stale of [...retiredFleet, ...fleetV1]) assert.doesNotMatch(copy, stale);
  assert.doesNotMatch(page.innerHTML, /github\.com\/Thurbeen\/fleet\b|install-release\.sh/, 'no link to the old Fleet repository');
  assert.doesNotMatch(page.innerHTML, /raw\.githubusercontent\.com\/Thurbeen\/fleet\//, 'no file from Fleet v1');
});

const pages = (dir = '_site') =>
  readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = `${dir}/${entry.name}`;
    if (entry.isDirectory()) return pages(path);
    return entry.name.endsWith('.html') ? [path.slice('_site/'.length)] : [];
  });

// The harnesses Fleet bundles, read from the profiles themselves.
const harnesses = [
  ...readFileSync('crates/fleet-api/src/harness/bundled.rs', 'utf8').matchAll(/^name = "([a-z-]+)"$/gm),
].map((m) => m[1]);

test('the site describes only what exists', () => {
  assert.ok(!existsSync('_site/docs/extensions.html'), 'no page for the removed hook entries');
  for (const path of pages()) {
    const page = built(path);
    for (const link of page.querySelectorAll('a[href]')) {
      const href = link.getAttribute('href');
      assert.doesNotMatch(href, /extensions\.html|#rmux$/, `${path} links ${href}`);
    }
    const copy = text(page);
    assert.doesNotMatch(copy, /soft_delete|agents\.toml/, `${path} names a removed key or file`);
    if (!['docs/configuration.html', 'docs/architecture.html'].includes(path)) assert.doesNotMatch(copy, /RMUX/, `${path} offers RMUX`);
  }
  const llms = readFileSync('_site/llms.txt', 'utf8');
  assert.doesNotMatch(llms, /extensions\.html|agents\.toml/);
  assert.ok(harnesses.length > 2, 'read the bundled harnesses');
  for (const copy of [text(built('docs/agents.html')), llms]) {
    for (const name of harnesses) assert.match(copy, new RegExp(`\\b${name}\\b`), `names ${name}`);
  }
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
  const plugins = ['info-panel', 'code-review', 'auto-continue', 'fleetlens', 'doom'];
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
