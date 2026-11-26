import assert from 'node:assert/strict';
import { runInNewContext } from 'node:vm';
import { test } from 'node:test';
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises';
import Eleventy from '@11ty/eleventy';
import { parse } from 'node-html-parser';

const searchItems = [
  {
    full_name: 'Thurbeen/thurbox-fixture',
    name: 'thurbox-fixture',
    owner: { login: 'Thurbeen' },
    html_url: 'https://github.com/Thurbeen/thurbox-fixture',
    description: 'A searchable pane <safe>.',
    stargazers_count: 2,
    pushed_at: '2026-09-01T08:00:00Z',
    created_at: '2026-01-01T08:00:00Z',
    default_branch: 'main',
    topics: ['thurbox-mods', 'lua'],
    fork: false,
    archived: false,
  },
  {
    full_name: 'contributor/zeta-extension',
    name: 'zeta-extension',
    owner: { login: 'contributor' },
    html_url: 'https://github.com/contributor/zeta-extension',
    description: 'An extension.',
    stargazers_count: 7,
    pushed_at: '2026-10-02T08:00:00Z',
    created_at: '2025-06-01T08:00:00Z',
    default_branch: 'main',
    topics: ['thurbox-mods'],
    fork: false,
    archived: false,
  },
];

const manifests = {
  'contributor/zeta-extension': `name = "zeta"
version = "v2.0.0"
requires_thurbox = ">=3.0.0"
pane = { source = "z.lua", path = "mods/50_z.lua" }

[gallery]
media = "shots/zeta.png"
media_alt = "Extension demo"
install = "./install.sh"
platforms = ["macOS"]
notes = "Clone the repository first."
`,
};

function mockGitHub(url) {
  const { hostname, pathname } = new URL(url);
  const reply = (status, body) => ({
    ok: status === 200,
    status,
    json: async () => body,
    text: async () => body,
  });
  if (pathname === '/search/repositories')
    return reply(200, { total_count: 2, incomplete_results: false, items: searchItems });
  if (hostname === 'api.github.com') return reply(404, {});
  const [, owner, repo] = pathname.split('/');
  if (pathname.endsWith('/mod.toml')) {
    const manifest = manifests[`${owner}/${repo}`];
    return manifest ? reply(200, manifest) : reply(404, '');
  }
  return reply(404, '');
}

test('site renders every discovered mod, its GitHub data, controls and escaped text', async () => {
  await mkdir('tmp', { recursive: true });
  const output = await mkdtemp('tmp/mods-site-');
  const realFetch = globalThis.fetch;
  const offline = process.env.THURBOX_MODS_OFFLINE;
  globalThis.fetch = async (url) => mockGitHub(url);
  delete process.env.THURBOX_MODS_OFFLINE;
  try {
    const site = new Eleventy('website', output, {
      source: 'cli',
      configPath: 'eleventy.config.js',
      config: (config) => config.setUseGitIgnore(false),
    });
    await site.write();
    const root = parse(await readFile(`${output}/mods.html`, 'utf8'));
    const cards = root.querySelectorAll('[data-plugin]');
    assert.deepEqual(
      cards.map((c) => c.querySelector('h2').textContent),
      ['zeta-extension', 'thurbox-fixture'],
      'most starred first',
    );
    const [zeta, fixture] = cards;

    assert.equal(fixture.getAttribute('data-badge'), 'official');
    assert.equal(fixture.querySelector('.plugin-badge').textContent, 'Official');
    assert.equal(zeta.getAttribute('data-badge'), 'community');
    assert.equal(zeta.querySelector('.plugin-badge').textContent, 'Community');

    assert.ok(fixture.innerHTML.includes('A searchable pane &lt;safe&gt;.'), 'description escaped');
    assert.equal(fixture.querySelector('.plugin-owner a').textContent, 'Thurbeen');
    assert.equal(fixture.querySelector('.plugin-stars').textContent, '2');
    assert.equal(fixture.querySelector('time').getAttribute('datetime'), '2026-09-01T08:00:00Z');
    assert.equal(fixture.querySelector('time').textContent, '2026-09-01');
    assert.deepEqual(
      fixture.querySelectorAll('.plugin-topics a').map((a) => a.textContent),
      ['lua'],
    );
    assert.equal(
      parse(fixture.querySelector('pre').innerHTML).textContent,
      'thurbox-cli mod install git+https://github.com/Thurbeen/thurbox-fixture',
    );
    assert.ok(fixture.querySelector('.plugin-preview-placeholder'));
    assert.equal(fixture.querySelector('.plugin-notes'), null);

    assert.equal(
      parse(zeta.querySelector('pre').innerHTML).textContent,
      'thurbox-cli mod install git+https://github.com/contributor/zeta-extension',
      'a community mod.toml cannot set the install command',
    );
    assert.match(zeta.textContent, /v2\.0\.0/);
    assert.match(zeta.textContent, />=3\.0\.0/);
    assert.match(zeta.textContent, /macOS/);
    assert.equal(zeta.querySelector('.plugin-notes').textContent, 'Clone the repository first.');
    const img = zeta.querySelector('.plugin-preview img');
    assert.equal(
      img.getAttribute('src'),
      'https://raw.githubusercontent.com/contributor/zeta-extension/main/shots/zeta.png',
    );
    assert.equal(img.getAttribute('alt'), 'Extension demo');
    assert.equal(img.getAttribute('loading'), 'lazy');

    for (const card of cards) {
      const install = card.querySelector('details.plugin-install');
      assert.ok(install, 'install command has a keyboard-accessible native reveal');
      assert.equal(install.hasAttribute('open'), false);
      assert.equal(install.querySelector('summary').textContent, 'Install command');
      assert.equal(card.querySelector('.plugin-avatar').getAttribute('alt'), '');
    }

    assert.equal(root.querySelector('#plugin-count').textContent, '2 mods');
    assert.equal(root.querySelectorAll('#plugin-sort option').length, 4);
    assert.equal(root.querySelectorAll('#plugin-badge option').length, 3);
    assert.ok(root.querySelector('label[for="plugin-search"]'));
    assert.ok(root.querySelector('label[for="plugin-sort"]'));
    assert.equal(root.querySelector('#plugin-count').getAttribute('aria-live'), 'polite');
    assert.ok(root.querySelector('a[aria-current="page"]'));
    assert.ok(root.querySelector('script[src="./js/mods.js"]'));

    const events = {};
    const form = root.querySelector('#plugin-filters');
    form.addEventListener = (event, handler) => {
      events[event] = handler;
    };
    const grid = root.querySelector('#plugin-grid');
    const search = root.querySelector('#plugin-search');
    const sort = root.querySelector('#plugin-sort');
    const badge = root.querySelector('#plugin-badge');
    search.value = badge.value = '';
    sort.value = 'popular';
    runInNewContext(await readFile('website/js/mods.js', 'utf8'), {
      document: {
        getElementById: (id) => root.querySelector(`#${id}`),
        querySelectorAll: (selector) => root.querySelectorAll(selector),
      },
      setTimeout: (handler) => handler(),
    });
    const order = () => grid.querySelectorAll('[data-plugin]').map((c) => c.querySelector('h2').textContent);
    assert.equal(form.hidden, false);
    assert.ok(cards.every((card) => !card.hidden));
    assert.deepEqual(order(), ['zeta-extension', 'thurbox-fixture']);

    sort.value = 'name';
    events.change();
    assert.deepEqual(order(), ['thurbox-fixture', 'zeta-extension']);
    sort.value = 'newest';
    events.change();
    assert.deepEqual(order(), ['thurbox-fixture', 'zeta-extension']);
    sort.value = 'updated';
    events.change();
    assert.deepEqual(order(), ['zeta-extension', 'thurbox-fixture']);

    search.value = '  LUA  ';
    events.input();
    assert.deepEqual(
      cards.map((card) => card.hidden),
      [true, false],
      'search matches topics',
    );
    search.value = '';
    badge.value = 'community';
    events.change();
    assert.deepEqual(
      cards.map((card) => card.hidden),
      [false, true],
    );
    search.value = 'nothing-matches';
    events.input();
    assert.equal(root.querySelector('#plugin-empty').hidden, false);
    search.value = badge.value = '';
    sort.value = 'popular';
    events.reset();
    assert.ok(cards.every((card) => !card.hidden));
    assert.deepEqual(order(), ['zeta-extension', 'thurbox-fixture']);
    assert.equal(root.querySelector('#plugin-count').textContent, '2 of 2 mods');
    assert.equal(root.querySelector('#plugin-empty').hidden, true);
  } finally {
    globalThis.fetch = realFetch;
    if (offline !== undefined) process.env.THURBOX_MODS_OFFLINE = offline;
    await rm(output, { recursive: true, force: true });
  }
});

const fixture = [
  {
    repo: 'https://github.com/Thurbeen/thurbox-fixture',
    install: 'git clone https://github.com/Thurbeen/thurbox-fixture\n./install.sh',
    platforms: ['Linux'],
    notes: 'Place its slot in layout.lua.',
  },
  {
    repo: 'https://github.com/example/extension',
    notes: 'Clone the repository first.',
    media: { url: 'https://example.com/demo.gif', alt: 'Extension demo' },
  },
  { repo: 'https://github.com/spam/ads', hidden: true },
];

test('registry schema rejects unknown fields, unsafe URLs, invalid values and duplicates', async () => {
  const { validateMods } = await import('./validate-mods.mjs');
  assert.deepEqual(validateMods(fixture), []);
  assert.deepEqual(validateMods([{ repo: 'https://github.com/example/bare' }]), []);
  assert.ok(validateMods([{ notes: 'no repo' }]).length, 'repo is required');
  for (const patch of [
    { repo: 'javascript:alert(1)' },
    { repo: 'https://example.com/repo' },
    { repo: 'https://github.com:8443/Thurbeen/thurbox-fixture' },
    { repo: 'https://github.com/Thurbeen/thurbox-fixture.git' },
    { badge: 'official' },
    { hidden: 'yes' },
    { install: '' },
    { notes: ' ' },
    { platforms: [] },
    { platforms: ['Linux', 'Linux'] },
    { media: { url: 'http://example.com/x.gif', alt: 'x' } },
    { media: { url: 'https://example.com/x.gif', alt: '' } },
  ]) {
    assert.ok(validateMods([{ ...fixture[0], ...patch }]).length, JSON.stringify(patch));
  }
  assert.ok(
    validateMods([fixture[0], { repo: fixture[0].repo.toUpperCase() }]).length,
    'duplicate repository',
  );
  assert.ok(validateMods({}).length);
  assert.ok(validateMods([null]).length);
});

test('link validation rejects a missing repository and unreachable media', async () => {
  const { checkLinks } = await import('./validate-mods.mjs');
  const seen = [];
  const errors = await checkLinks(fixture, async (url) => {
    seen.push(url);
    return { ok: !url.includes('extension'), status: 404 };
  });
  assert.ok(seen.includes(fixture[1].repo));
  assert.ok(seen.includes(fixture[1].media.url));
  assert.ok(!seen.includes(fixture[2].repo), 'a hidden repository is not checked');
  assert.ok(errors.some((e) => e.includes(fixture[1].repo)));
  assert.ok(!errors.some((e) => e.includes(fixture[0].repo)));
  const unavailable = await checkLinks(fixture, async () => {
    throw new Error('offline');
  });
  assert.ok(unavailable.length);
});

test('catalog inherits the website single theme', async () => {
  const css = await readFile('website/css/mods.css', 'utf8');
  assert.doesNotMatch(css, /prefers-color-scheme|:root/);
});
