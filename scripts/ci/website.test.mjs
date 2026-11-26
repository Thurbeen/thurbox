import assert from 'node:assert/strict';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { posix } from 'node:path';
import { test } from 'node:test';
import { parse } from 'node-html-parser';
import { THEMES, overviewPath, overviewSvg } from '../docs/overview-svg.mjs';
import { fleetLoopPath, fleetLoopSvg } from '../docs/fleet-loop-svg.mjs';
import { RENDERED_DOCS } from '../../eleventy.config.js';

const built = (path) => parse(readFileSync(`_site/${path}`, 'utf8'));
const text = (node) => node.textContent.replace(/\s+/g, ' ');

test('mod catalog lists Voice with its repository, install steps and data flow', () => {
  const repo = 'https://github.com/TMSCH/thurbox-voice';
  const card = built('mods.html')
    .querySelectorAll('[data-plugin]')
    .find((c) => c.querySelector('h2 a')?.getAttribute('href') === repo);
  assert.ok(card, 'a catalog card links to the thurbox-voice repository');
  assert.equal(card.querySelector('h2').textContent, 'thurbox-voice');
  assert.equal(card.getAttribute('data-badge'), 'community');
  assert.ok(card.querySelector(`a[href="${repo}#readme"]`), 'setup link goes to the README');
  const install = text(parse(card.querySelector('pre').innerHTML));
  assert.match(install, /git clone https:\/\/github\.com\/TMSCH\/thurbox-voice/);
  assert.match(install, /thurbox-cli mod install git\+https:\/\/github\.com\/TMSCH\/thurbox-voice/);
  const copy = text(card);
  assert.match(copy, /Audio stays local/i);
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

const fleetV1 = [/uv run fleet/, /\/fleet-onboarding/, /Mission Control/, /Kanban/, /result\.md/, /--kind result/];

test('orchestration page leads with Fleet, its reference on this site and Fleet Lens', () => {
  const page = built('docs/orchestration.html');
  assert.ok(page.querySelector('a[href="fleet/index.html"]'), "links Fleet's reference");
  assert.ok(page.querySelector('a[href="https://github.com/Thurbeen/thurbox-fleetlens"]'), 'links Fleet Lens');
  const h2s = page.querySelectorAll('h2').map((h) => h.getAttribute('id'));
  assert.equal(h2s[0], 'fleet', 'Fleet is the first section');
  const copy = text(page);
  const code = page.querySelectorAll('pre').map((pre) => text(parse(pre.innerHTML))).join('\n');
  assert.match(code, /curl -fsSL https:\/\/raw\.githubusercontent\.com\/Thurbeen\/thurbox\/main\/scripts\/install\.sh \| sh/);
  assert.match(code, /thurbox-cli mod install git\+https:\/\/github\.com\/Thurbeen\/thurbox-fleetlens/);
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
    if (!['docs/configuration.html', 'docs/architecture.html', 'docs/runtime/index.html', 'docs/runtime/runtime-mux.html', 'docs/thurbox/architecture.html'].includes(path)) assert.doesNotMatch(copy, /RMUX/, `${path} offers RMUX`);
  }
  const llms = readFileSync('_site/llms.txt', 'utf8');
  assert.doesNotMatch(llms, /extensions\.html|agents\.toml/);
  assert.ok(harnesses.length > 2, 'read the bundled harnesses');
  for (const copy of [text(built('docs/agents.html')), llms]) {
    for (const name of harnesses) assert.match(copy, new RegExp(`\\b${name}\\b`), `names ${name}`);
  }
});

test('mod catalog asks authors for the thurbox-mods topic and links its page', () => {
  const page = built('mods.html');
  const topic = page.querySelector('a[href="https://github.com/topics/thurbox-mods"]');
  assert.ok(topic, 'links the thurbox-mods topic page');
  assert.equal(topic.text, 'thurbox-mods');
  assert.doesNotMatch(text(page), /\bthurbox-plugin\b(?!-)/);
  const contributing = readFileSync('CONTRIBUTING.md', 'utf8');
  assert.match(contributing, /\[`thurbox-mods`\]\(https:\/\/github\.com\/topics\/thurbox-mods\)/);
  assert.doesNotMatch(contributing, /`thurbox-plugin`/);
});

test('mod catalog lists Fleet Lens in place of the Fleet v1 extension', () => {
  const cards = built('mods.html').querySelectorAll('[data-plugin]');
  const repos = cards.map((c) => c.querySelector('h2 a').getAttribute('href'));
  assert.ok(!repos.includes('https://github.com/Thurbeen/fleet'), 'Fleet v1 is not a mod');
  const repo = 'https://github.com/Thurbeen/thurbox-fleetlens';
  const card = cards.find((c) => c.querySelector('h2 a').getAttribute('href') === repo);
  assert.ok(card, 'a catalog card links to thurbox-fleetlens');
  assert.equal(card.getAttribute('data-badge'), 'official');
  assert.match(text(parse(card.querySelector('pre').innerHTML)), /thurbox-cli mod install git\+https:\/\/github\.com\/Thurbeen\/thurbox-fleetlens/);
  assert.equal(
    card.querySelector('.plugin-preview img').getAttribute('src'),
    'https://raw.githubusercontent.com/Thurbeen/thurbox-fleetlens/main/media/demo.gif',
  );
});

test('mod gallery is discovered by topic and documents the mod.toml listing convention', () => {
  const page = built('mods.html');
  const copy = text(page);
  assert.match(copy, /discovered automatically/i);
  assert.match(copy, /not reviewed/i, 'community listings are flagged as unreviewed');
  const toml = text(parse(page.querySelector('#submit-mod pre').innerHTML));
  for (const field of ['[gallery]', 'media', 'media_alt', 'platforms', 'notes'])
    assert.ok(toml.includes(field), field);
  const contributing = readFileSync('CONTRIBUTING.md', 'utf8');
  assert.match(contributing, /## Mod gallery/);
  assert.match(contributing, /\[gallery\]/);
  assert.match(contributing, /website\/mods-snapshot\.json/);
});

test('Fleet plugins page lists fleet-reviewer with its kinds, beside the Mods page', () => {
  const page = built('fleet-plugins.html');
  const card = page
    .querySelectorAll('[data-plugin]')
    .find((c) => c.querySelector('h2 a')?.getAttribute('href') === 'https://github.com/Thurbeen/fleet-reviewer');
  assert.ok(card, 'a card links to fleet-reviewer');
  assert.equal(card.getAttribute('data-badge'), 'official');
  assert.deepEqual(
    card.querySelectorAll('.plugin-kinds code').map((c) => c.textContent),
    ['ReviewSource', 'Review'],
  );
  assert.match(text(parse(card.querySelector('pre').innerHTML)), /git clone https:\/\/github\.com\/Thurbeen\/fleet-reviewer/);
  const copy = text(page);
  assert.match(copy, /discovered automatically/i);
  assert.match(copy, /not reviewed/i);
  assert.doesNotMatch(copy, /\bmods? (adds?|extends?) Fleet/i);
  const navOrder = page.querySelectorAll('.nav-links a').map((a) => a.textContent.trim());
  assert.equal(navOrder.indexOf('Fleet plugins'), navOrder.indexOf('Mods') + 1);
  const guide = built('docs/fleet/plugins.html');
  assert.ok(guide.querySelector('a[href="https://thurbox.thurbeen.eu/fleet-plugins.html"]'));
  assert.match(text(guide), /fleet-plugin\.toml/);
  assert.match(text(guide), /A Fleet plugin is not a thurbox mod/);
  assert.match(readFileSync('CONTRIBUTING.md', 'utf8'), /## Fleet plugin gallery/);
});

test('the site rebuilds every hour and skips a scheduled deploy when the galleries did not change', () => {
  const workflow = readFileSync('.github/workflows/pages.yml', 'utf8');
  const cron = workflow.match(/- cron: '([^']+)'/)[1].split(' ');
  assert.equal(cron[1], '*', 'every hour');
  assert.notEqual(cron[0], '0', 'off the top of the hour, when GitHub delays schedules');
  assert.match(workflow, /galleries\.sha256/);
  assert.match(workflow, /github\.event_name != 'schedule'/);
  assert.match(readFileSync('_site/galleries.sha256', 'utf8'), /^[0-9a-f]{64}\n$/);
  for (const page of ['mods.html', 'fleet-plugins.html'])
    assert.match(text(built(page)), /rebuilds every hour/, page);
});

test('homepage presents Fleet and thurbox apart, links each official mod, and stays light on commands', () => {
  const home = built('index.html');
  const install = 'curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh';
  const code = (node) =>
    node.querySelectorAll('pre').map((pre) => parse(pre.innerHTML).textContent.trim().split(/\s*\n\s*/));

  const bands = home.querySelectorAll('section.product').map((s) => s.getAttribute('id'));
  assert.deepEqual(bands, ['fleet', 'thurbox'], 'the Fleet band comes before the thurbox band');
  const ctas = home.querySelectorAll('.hero-ctas a').map((a) => a.getAttribute('href'));
  assert.deepEqual(ctas, ['#fleet', '#thurbox'], 'the hero leads with Fleet');
  const starts = home.querySelectorAll('.get-started').map((s) => s.getAttribute('id'));
  assert.deepEqual(starts, ['fleet-start', 'thurbox-start'], 'Fleet gets started first');

  const fleet = home.querySelector('#fleet');
  assert.match(text(fleet), /works on its own/i, 'Fleet is said to run without thurbox');
  const fleetStart = code(home.querySelector('#fleet-start'));
  assert.equal(fleetStart.length, 1);
  assert.deepEqual(fleetStart[0], [`${install} -s -- --fleet-only`, 'fleet init', 'fleet doctor', 'fleet up']);
  const cli = readFileSync('crates/fleet-cli/src/main.rs', 'utf8');
  for (const line of code(fleet).flat().filter((l) => l.startsWith('fleet '))) {
    const verb = line.split(' ')[1];
    const variant = verb[0].toUpperCase() + verb.slice(1);
    assert.match(cli, new RegExp(`^    ${variant}\\b`, 'm'), `${line}: fleet ${verb} exists`);
  }

  const thurboxStart = code(home.querySelector('#thurbox-start'));
  assert.deepEqual(thurboxStart, [[install, 'thurbox']]);
  assert.ok(home.querySelector('#thurbox video'), 'the demo is in the thurbox band');

  const section = home.querySelector('#plugins');
  const mods = ['info-panel', 'code-review', 'auto-continue', 'fleetlens', 'doom'];
  for (const name of mods) {
    const repo = `https://github.com/Thurbeen/thurbox-${name}`;
    const link = section.querySelector(`a[href="${repo}"]`);
    assert.ok(link, `links ${repo}`);
    assert.ok(text(link.parentNode).trim().length > `thurbox-${name}`.length, `${name} says what it does`);
  }
  assert.ok(section.querySelector('a[href="mods.html"]'), 'points at the full catalog');
  assert.ok(home.querySelector('#thurbox #plugins'), 'the mods sit under thurbox');
  assert.doesNotMatch(home.innerHTML, /thurbox-discord|thurbox-plugin-orchestrator|thurbox-ask/);
  const codeBlocks = home.querySelectorAll('pre').filter((pre) => /language-/.test(pre.innerHTML));
  assert.equal(codeBlocks.length, 3, 'two install blocks and the Fleet example');
  assert.doesNotMatch(text(home), /thurbox-cli (mod|plugin|session) /);
  assert.ok(home.querySelector('a[href="docs/installation.html"]'), 'other install routes are linked');
  assert.ok(home.querySelector('a[href="docs/fleet-install.html"]'), 'installing Fleet alone is linked');
  const fit = home.querySelector('#how-they-fit');
  const order = home.querySelectorAll('section[id]').map((s) => s.getAttribute('id'));
  assert.ok(order.indexOf('how-they-fit') > order.indexOf('thurbox'), 'the scenes follow the product bands');
  const scenes = fit.querySelectorAll('[data-story] svg');
  assert.equal(scenes.length, 4, 'one scene each for places, the reconcile loop, agents working together and change requests');
  for (const svg of scenes) {
    const labels = svg.getAttribute('aria-labelledby').split(' ');
    assert.deepEqual(labels.map((id) => svg.querySelector(`#${id}`)?.tagName), ['TITLE', 'DESC'], 'each scene has a title and a description');
  }
  const [places, loop, team, reviews] = scenes.map((svg) => text(svg));

  const benefits = fit.querySelectorAll('.story__benefits .card');
  assert.equal(benefits.length, 5, 'five differentiators sit above the scenes');
  for (const card of benefits) {
    const target = card.querySelector('a')?.getAttribute('href') ?? '';
    assert.ok(target.startsWith('#') && fit.querySelector(target), `"${text(card.querySelector('h3'))}" links to the part that shows it`);
  }
  const headlines = benefits.map((card) => text(card.querySelector('h3'))).join(' | ');
  assert.doesNotMatch(headlines, /one screen/i, 'one screen for every agent is no longer a headline');
  assert.doesNotMatch(text(fit), /\bmirror/i, 'the pitch says what Fleet does, not "mirror"');

  const placeKinds = readFileSync('crates/runtime-mux/src/host/host_def.rs', 'utf8');
  for (const kind of ['Ssh', 'Wsl']) assert.match(placeKinds, new RegExp(`^    ${kind},`, 'm'), `${kind} is a place kind runtime-mux has`);
  for (const word of ['thurbox', 'local', 'ssh', 'wsl', 'mux/box', 'workspace', 'session']) {
    assert.match(places, new RegExp(word), `the places scene reads "${word}" as text`);
  }
  assert.doesNotMatch(text(home), /herdr|docker|podman|kubernetes pod|devcontainer/i, 'no place kind Fleet does not ship is claimed');

  for (const word of ['Task', 'spec', 'status', 'controllers', 'fleet up', 'observe', 'compare', 'act', 'runtime-mux', 'workspace', 'session', 'TaskConfig', 'CronTask', 'ChangeRequest']) {
    assert.match(loop, new RegExp(word), `the reconcile scene reads "${word}" as text`);
  }
  assert.match(text(fit.querySelector('#story-reconcile')), /Kubernetes/, 'the reconcile loop is compared to Kubernetes');
  const kinds = readFileSync('docs/fleet/README.md', 'utf8');
  for (const kind of ['Task', 'TaskConfig', 'EnvProvider', 'ChangeRequest', 'CronTask', 'Board', 'Conversation']) {
    assert.match(kinds, new RegExp(`^\\| \\[${kind}\\]\\(resources/`, 'm'), `${kind} is a kind Fleet documents`);
  }
  const phases = readFileSync('docs/fleet/resources/task.md', 'utf8');
  for (const phase of ['Pending', 'Spawning', 'Briefed']) assert.match(phases, new RegExp(`\`${phase}\``), `${phase} is a Task phase`);

  for (const word of ['Slack', 'Discord', 'gateway', 'lead', 'child', 'inbox', 'Question', 'Board']) {
    assert.match(team, new RegExp(word), `the team scene reads "${word}" as text`);
  }
  assert.doesNotMatch(text(fit), /Telegram|Teams|Matrix|Mattermost/, 'only the chat platforms Fleet has adapters for are shown');
  const adapters = readdirSync('crates/fleet-gateway/src').map((file) => file.replace(/\.rs$/, ''));
  for (const platform of ['slack', 'discord']) assert.ok(adapters.includes(platform), `${platform} has a gateway adapter`);

  const forges = readdirSync('crates/fleet-forge/src').map((file) => file.replace(/\.rs$/, ''));
  for (const [forge, adapter] of [['GitHub', 'github'], ['GitLab', 'gitlab'], ['Bitbucket', 'bitbucket'], ['Azure DevOps', 'azure_devops']]) {
    assert.ok(forges.includes(adapter), `${forge} has a forge adapter`);
    assert.match(text(fit.querySelector('#story-reviews')), new RegExp(forge), `the change-request part names ${forge}`);
  }
  for (const word of ['checks', 'review', 'comment', 'inbox', 'controllers']) {
    assert.match(reviews, new RegExp(word), `the change-request scene reads "${word}" as text`);
  }

  const extend = fit.querySelector('#story-extend');
  for (const word of ['plugin', 'kind', 'controller', 'CRD', 'harness', 'runtime protocol', 'Lua']) {
    assert.match(text(extend), new RegExp(word), `the extension part reads "${word}"`);
  }
  for (const href of ['docs/fleet/plugins.html', 'mods.html', 'docs/runtime/index.html', 'docs/agents.html']) {
    assert.ok(extend.querySelector(`a[href="${href}"]`), `the extension part links ${href}`);
  }
  assert.ok(existsSync('examples/fleet/plugin/files.py'), 'the example Fleet plugin exists');
  const harnesses = readFileSync('crates/fleet-api/src/harness/bundled.rs', 'utf8');
  for (const harness of ['claude', 'codex', 'opencode']) {
    assert.match(harnesses, new RegExp(`^name = "${harness}"$`, 'm'), `${harness} is a bundled harness profile`);
    assert.match(text(extend), new RegExp(harness), `the extension part names ${harness}`);
  }
  for (const step of fit.querySelectorAll('[data-story]')) {
    const numbers = step.querySelectorAll('[data-step]').map((s) => Number(s.getAttribute('data-step')));
    assert.deepEqual(numbers, numbers.map((_, i) => i + 1), 'each story numbers its steps from 1');
  }
  assert.doesNotMatch(text(fit), / \./, 'no space before a period');
});

test('the story scenes draw in the site theme: squared panels, theme tokens, numbered steps', () => {
  const fit = built('index.html').querySelector('#how-they-fit');
  const rounded = fit.querySelectorAll('[data-story] svg rect').filter((rect) => rect.hasAttribute('rx'));
  assert.equal(rounded.length, 0, 'scene panels are squared like every other panel');
  for (const step of fit.querySelectorAll('[data-step]')) {
    const badge = step.querySelector('h3 .step-number');
    assert.equal(badge?.textContent, step.getAttribute('data-step'), 'each step heading carries its number in a step badge');
    assert.equal(badge.getAttribute('aria-hidden'), 'true', 'the list already numbers the step for a screen reader');
  }
  const css = readFileSync('website/css/landing.css', 'utf8');
  const story = css.slice(css.indexOf('#how-they-fit {'), css.indexOf('/* ---- Back to top'));
  assert.ok(story.length > 0, 'the story rules are found');
  assert.doesNotMatch(story, /#[0-9a-f]{3,8}\b|rgba?\(|hsla?\(/i, 'story colours come from theme variables');
  assert.doesNotMatch(story, /font-family:(?!\s*var\()/, 'story fonts come from theme variables');
  assert.doesNotMatch(story, /\.story__step[^{]*\{[^}]*opacity/, 'a step is never dimmed below readable contrast');
  for (const step of [1, 2, 3, 4]) {
    assert.match(story, new RegExp(`\\.story--live:not\\(\\.at-${step}\\) \\.s${step}:not\\(\\.st-pulse\\)`), `a pulse of step ${step} stays hidden until its step, not dimmed with the scene`);
  }
  const paused = story.match(/\.story--live\.is-visible\.is-paused\[data-at\] \.story__svg \*\s*\{([^}]*)\}/);
  assert.ok(paused, 'a paused scene has a rule over everything its figure draws');
  assert.match(paused[1], /animation-play-state:\s*paused/, 'Pause stops every animation in the figure, not only the step timer');
});

test('the docs sidebar gives Fleet and thurbox a group each', () => {
  const groups = built('docs/index.html').querySelectorAll('.docs-sidebar h4').map((h) => h.textContent.trim());
  assert.deepEqual(groups.slice(0, 2), ['Fleet', 'Thurbox']);
  const workspaceLinks = built('docs/index.html')
    .querySelectorAll('.docs-sidebar a')
    .filter((a) => a.getAttribute('href') === 'fleet/resources/workspace.html');
  assert.equal(workspaceLinks.length, 1, 'Workspaces is listed once, as its own entry');
  assert.ok(
    built('docs/index.html').querySelector('.docs-sidebar__sub a[href="fleet/plugins.html"]'),
    'Controllers lists the plugin guide',
  );
  const subs = built('docs/index.html').querySelectorAll('.docs-sidebar__sub a').map((a) => a.getAttribute('href'));
  for (const dir of ['resources', 'controllers']) {
    for (const file of readdirSync(`docs/fleet/${dir}`).filter((f) => f.endsWith('.md') && f !== 'workspace.md')) {
      assert.ok(subs.includes(`fleet/${dir}/${file.replace(/\.md$/, '.html')}`), `the sidebar lists ${dir}/${file}`);
    }
  }
  const moved = built('docs/features.html');
  for (const page of ['fleet-features.html', 'thurbox-features.html']) {
    assert.ok(moved.querySelector(`.docs-content a[href="${page}"]`), `docs/features.html links ${page}`);
  }
  const script = moved.querySelector('#features-moved').textContent;
  const targets = [...script.matchAll(/'?([a-z-]+)'?: '([a-z-]+\.html)(?:#([a-z-]+))?'/g)];
  const oldIds = ['agent-definitions', 'automations', 'customizable-interface', 'feature-flags', 'git-worktrees',
    'global-search', 'headless-cli', 'inter-session-messages', 'mouse-navigation', 'os-notifications',
    'parent-sessions', 'remote-ssh-sessions', 'responsive-ui', 'session-orchestration', 'session-persistence',
    'themes', 'where-surfaces-live'];
  assert.deepEqual(targets.map((m) => m[1]).sort(), oldIds);
  for (const [, from, page, id] of targets) {
    if (id) assert.ok(built(`docs/${page}`).querySelector(`[id="${id}"]`), `#${from} -> ${page}#${id} exists`);
  }
  for (const page of ['fleet-get-started', 'fleet', 'fleet-places', 'fleet-install', 'fleet-features', 'thurbox-get-started', 'thurbox', 'thurbox-features']) {
    assert.ok(existsSync(`_site/docs/${page}.html`), `docs/${page}.html exists`);
  }
});

const repoDocs = ['docs/fleet', 'docs/runtime', 'docs/thurbox/mods.md', 'docs/thurbox/kernel.md'].flatMap((entry) =>
  (entry.endsWith('.md') ? [entry.split('/').pop()] : readdirSync(entry, { recursive: true }))
    .map((file) => (entry.endsWith('.md') ? [entry.replace(/\/[^/]+$/, ''), file] : [entry, file]))
    .filter(([, file]) => file.endsWith('.md'))
    .map(([dir, file]) => [`${dir}/${file}`, `${dir}/${file}`.replace(/(^|\/)README\.md$/, '$1index.md').replace(/\.md$/, '.html')]),
);

test('no page links to the documentation on GitHub', () => {
  for (const path of pages()) {
    for (const link of built(path).querySelectorAll('a[href]')) {
      const href = link.getAttribute('href');
      assert.doesNotMatch(href, /github\.com\/Thurbeen\/thurbox\/(tree|blob)\/main\/docs\b/, `${path} links ${href}`);
    }
  }
  assert.doesNotMatch(readFileSync('_site/llms.txt', 'utf8'), /github\.com\/Thurbeen\/thurbox\/(tree|blob)\/main\/docs\b/);
});

test("every page of Fleet's docs is on the site, and its links and images resolve", () => {
  assert.ok(repoDocs.some(([src]) => src === 'docs/fleet/resources/task.md'), 'read docs/fleet');
  const parsed = new Map();
  const ids = (path) => {
    if (!parsed.has(path)) parsed.set(path, new Set(built(path).querySelectorAll('[id]').map((n) => n.id)));
    return parsed.get(path);
  };
  for (const [src, path] of repoDocs) {
    assert.ok(existsSync(`_site/${path}`), `${src} is rendered at ${path}`);
    const page = built(path);
    assert.ok(page.querySelector('.docs-content h1'), `${path} has its title`);
    const dir = path.split('/').slice(0, -1).join('/');
    const refs = [
      ...page.querySelectorAll('a[href]').map((a) => a.getAttribute('href')),
      ...page.querySelectorAll('.docs-content img[src]').map((img) => img.getAttribute('src')),
    ];
    for (const ref of refs) {
      if (/^[a-z]+:/i.test(ref)) continue;
      const [file, fragment] = ref.split('#');
      const target = file ? new URL(file, `file:///${dir}/`).pathname.slice(1) : path;
      assert.ok(existsSync(`_site/${target}`), `${path}: ${ref} is not built`);
      if (fragment && target.endsWith('.html')) {
        assert.ok(ids(target).has(fragment), `${path}: ${ref} names no id on ${target}`);
      }
    }
  }
});

test('the docs sidebar reaches the rendered Fleet reference from every depth', () => {
  for (const path of ['docs/index.html', 'docs/fleet/index.html', 'docs/fleet/resources/task.html']) {
    const page = built(path);
    const dir = path.split('/').slice(0, -1).join('/');
    const hrefs = page.querySelectorAll('.docs-sidebar a').map((a) => a.getAttribute('href'));
    assert.ok(hrefs.length > 0, `${path} has a sidebar`);
    for (const href of hrefs) {
      const target = new URL(href.split('#')[0], `file:///${dir}/`).pathname.slice(1);
      assert.ok(existsSync(`_site/${target}`), `${path}: sidebar ${href} is not built`);
    }
    assert.ok(hrefs.some((h) => h.endsWith('fleet/index.html')), `${path} links the Fleet reference`);
  }
  const forge = built('docs/fleet/controllers/forge.html');
  assert.ok(forge.querySelectorAll('.docs-toc a').length > 2, 'the on-this-page rail lists the headings');
  assert.ok(forge.querySelector('.docs-content img[src="forge.svg"]'), 'the state diagram is beside the page');
});

test('the site redeploys when a rendered doc changes', () => {
  const workflow = readFileSync('.github/workflows/pages.yml', 'utf8');
  const paths = [...workflow.matchAll(/^\s+- '([^']+)'$/gm)].map((m) => m[1]);
  for (const entry of RENDERED_DOCS) {
    const want = entry.endsWith('.md') ? entry : `${entry}/**`;
    assert.ok(paths.includes(want), `pages.yml's push paths list ${want}`);
  }
});

test("the home page's grid tracks shrink to a phone's width", () => {
  const css = readFileSync('website/css/landing.css', 'utf8');
  assert.doesNotMatch(css, /minmax\(\s*\d+(\.\d+)?(rem|px|em)\s*,/, 'a minmax() minimum is wrapped in min(…, 100%)');
});

test('every link to a section on the site lands on an element with that id', () => {
  for (const path of pages()) {
    const page = built(path);
    const base = path.includes('/') ? path.slice(0, path.lastIndexOf('/') + 1) : '';
    for (const link of page.querySelectorAll('a[href*="#"]')) {
      const href = link.getAttribute('href');
      if (/^[a-z][a-z0-9+.-]*:/i.test(href) || href.startsWith('//')) continue;
      const [target, fragment] = href.split('#');
      if (!fragment) continue;
      const file = target ? posix.normalize(base + target) : path;
      if (!file.endsWith('.html') || !existsSync(`_site/${file}`)) continue;
      assert.ok(built(file).querySelector(`[id="${fragment}"]`), `${path}: ${href} names no id on ${file}`);
    }
  }
});

test('the overview diagrams are what their generator draws', () => {
  for (const theme of THEMES) {
    assert.equal(
      readFileSync(overviewPath(theme), 'utf8'),
      overviewSvg(theme),
      `${overviewPath(theme)} is stale: run node scripts/docs/overview-svg.mjs`,
    );
  }
});

test('the Fleet loop diagrams are what their generator draws', () => {
  for (const theme of THEMES) {
    assert.equal(
      readFileSync(fleetLoopPath(theme), 'utf8'),
      fleetLoopSvg(theme),
      `${fleetLoopPath(theme)} is stale: run node scripts/docs/fleet-loop-svg.mjs`,
    );
  }
});

test('every page shows the overview in the theme of its reader', () => {
  for (const page of ['docs/architecture.html', 'docs/configuration.html', 'docs/fleet-places.html']) {
    const picture = built(page).querySelector('picture');
    assert.equal(
      picture?.querySelector('source[media="(prefers-color-scheme: dark)"]')?.getAttribute('srcset'),
      '../assets/overview-dark.svg',
      `${page} has the dark overview`,
    );
    assert.equal(picture.querySelector('img').getAttribute('src'), '../assets/overview-light.svg', `${page} falls back to the light overview`);
  }
});

test('the old plugin URLs redirect to the mod pages', () => {
  for (const [old, target] of [
    ['plugins.html', 'mods.html'],
    ['docs/thurbox/plugins.html', 'mods.html'],
  ]) {
    const page = built(old);
    assert.match(page.querySelector('meta[http-equiv="refresh"]')?.getAttribute('content') ?? '', new RegExp(`^0; url=${target}$`), `${old} refreshes to ${target}`);
    assert.ok(page.querySelector(`link[rel="canonical"][href$="${target}"]`), `${old} names its canonical page`);
    assert.ok(page.querySelector(`a[href="${target}"]`), `${old} links ${target}`);
    const dir = old.split('/').slice(0, -1).join('/');
    assert.ok(existsSync(`_site/${posix.join(dir, target)}`), `${old} points at a built page`);
  }
  const sitemap = readFileSync('_site/sitemap.xml', 'utf8');
  assert.doesNotMatch(sitemap, /\.eu\/plugins\.html|thurbox\/plugins\.html/, 'the sitemap lists only the mod pages');
});

test("the site calls thurbox's interface add-ons mods", () => {
  for (const path of pages()) {
    const copy = text(built(path));
    assert.doesNotMatch(copy, /thurbox-cli plugin\b|perf --plugins/, `${path} shows a plugin command`);
  }
  assert.doesNotMatch(readFileSync('_site/llms.txt', 'utf8'), /Lua plugin|plugin kernel|thurbox-cli plugin\b/);
  const nav = built('index.html').querySelectorAll('nav a').map((a) => text(a).trim());
  assert.ok(nav.includes('Mods'), 'the top nav names Mods');
  assert.ok(!nav.includes('Plugins'), 'the top nav names no Plugins');
});
