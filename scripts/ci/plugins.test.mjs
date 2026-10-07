import assert from 'node:assert/strict';
import { runInNewContext } from 'node:vm';
import { test } from 'node:test';
import { cp, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import Eleventy from '@11ty/eleventy';
import { parse } from 'node-html-parser';

const fixture = [
  {
    name: 'Fixture pane <safe>',
    description: 'A searchable pane.',
    author: 'Example',
    badge: 'official',
    repo: 'https://github.com/Thurbeen/thurbox-fixture',
    kind: 'pane',
    platforms: ['Linux'],
    updated: '2026-09-01',
    install: 'thurbox-cli plugin install git+https://github.com/Thurbeen/thurbox-fixture',
    notes: 'Place its slot in layout.lua.',
  },
  {
    name: 'Fixture extension',
    description: 'An extension.',
    author: 'Contributor',
    badge: 'community',
    repo: 'https://github.com/example/extension',
    kind: 'extension',
    platforms: ['macOS'],
    updated: '2026-09-02',
    install: './install.sh',
    notes: 'Clone the repository first.',
    media: { url: 'https://example.com/demo.gif', alt: 'Extension demo' },
  },
];

// The fixture goes through the same template and transforms as the production catalog.
test('site builds every fixture entry, badges, controls and escaped text', async () => {
  await mkdir('tmp', { recursive: true });
  const scratch = await mkdtemp('tmp/plugins-site-');
  const output = `${scratch}/output`;
  try {
    await cp('website', `${scratch}/website`, { recursive: true });
    await writeFile(`${scratch}/website/_data/plugins.json`, JSON.stringify(fixture));
    const site = new Eleventy(`${scratch}/website`, output, {
      source: 'cli',
      configPath: 'eleventy.config.js',
      config: (config) => config.setUseGitIgnore(false),
    });
    await site.write();
    const root = parse(await readFile(`${output}/plugins.html`, 'utf8'));
    const cards = root.querySelectorAll('[data-plugin]');
    assert.equal(cards.length, fixture.length);
    fixture.forEach((entry, i) => {
      assert.equal(cards[i].querySelector('h2').textContent, entry.name);
      assert.equal(cards[i].getAttribute('data-kind'), entry.kind);
      assert.equal(cards[i].getAttribute('data-badge'), entry.badge);
      assert.equal(parse(cards[i].querySelector('pre').innerHTML).textContent, entry.install);
      assert.ok(cards[i].textContent.includes(entry.author));
      const install = cards[i].querySelector('details.plugin-install');
      assert.ok(install, 'install command has a keyboard-accessible native reveal');
      assert.equal(install.hasAttribute('open'), false);
      assert.equal(install.querySelector('summary').textContent, 'Install command');
      assert.ok(install.querySelector('pre'));
      const preview = cards[i].querySelector('.plugin-preview');
      assert.ok(preview);
      assert.notEqual(preview.tagName, 'DETAILS');
      for (let ancestor = preview; ancestor !== cards[i]; ancestor = ancestor.parentNode) {
        assert.notEqual(ancestor.tagName, 'DETAILS', 'preview is visible before interaction');
        assert.equal(ancestor.hasAttribute('hidden'), false);
      }
      if (entry.media) {
        assert.equal(preview.querySelector('img').getAttribute('src'), entry.media.url);
        assert.equal(preview.querySelector('img').getAttribute('loading'), 'lazy');
      } else {
        assert.ok(preview.querySelector('.plugin-preview-placeholder'));
      }
    });
    assert.equal(root.querySelectorAll('#plugin-kind option').length, 6);
    assert.equal(root.querySelectorAll('#plugin-badge option').length, 3);
    assert.ok(root.querySelector('label[for="plugin-search"]'));
    assert.equal(root.querySelector('#plugin-count').getAttribute('aria-live'), 'polite');
    assert.ok(root.querySelector('a[aria-current="page"]'));
    assert.equal(root.querySelector('img[alt="Extension demo"]').getAttribute('loading'), 'lazy');
    assert.ok(root.querySelector('script[src="./js/plugins.js"]'));
    const events = {};
    const form = root.querySelector('#plugin-filters');
    form.addEventListener = (event, handler) => {
      events[event] = handler;
    };
    const search = root.querySelector('#plugin-search');
    const kind = root.querySelector('#plugin-kind');
    const badge = root.querySelector('#plugin-badge');
    search.value = kind.value = badge.value = '';
    runInNewContext(await readFile('website/js/plugins.js', 'utf8'), {
      document: {
        getElementById: (id) => root.querySelector(`#${id}`),
        querySelectorAll: (selector) => root.querySelectorAll(selector),
      },
      setTimeout: (handler) => handler(),
    });
    assert.equal(form.hidden, false);
    assert.ok(cards.every((card) => !card.hidden));
    search.value = '  CONTRIBUTOR  ';
    events.input();
    assert.deepEqual(
      cards.map((card) => card.hidden),
      [true, false],
    );
    kind.value = 'pane';
    events.change();
    assert.ok(cards.every((card) => card.hidden));
    assert.equal(root.querySelector('#plugin-empty').hidden, false);
    search.value = kind.value = '';
    badge.value = 'official';
    events.change();
    assert.deepEqual(
      cards.map((card) => card.hidden),
      [false, true],
    );
    search.value = kind.value = badge.value = '';
    events.reset();
    assert.ok(cards.every((card) => !card.hidden));
    assert.equal(root.querySelector('#plugin-count').textContent, '2 of 2 plugins');
    assert.equal(root.querySelector('#plugin-empty').hidden, true);
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
});

test('catalog schema rejects missing fields, unsafe URLs, invalid values and duplicates', async () => {
  const { validatePlugins } = await import('./validate-plugins.mjs');
  assert.deepEqual(validatePlugins(fixture), []);
  for (const field of [
    'name',
    'description',
    'author',
    'badge',
    'repo',
    'kind',
    'platforms',
    'updated',
    'install',
    'notes',
  ]) {
    const entry = { ...fixture[0] };
    delete entry[field];
    assert.ok(validatePlugins([entry]).length, field);
  }
  for (const patch of [
    { repo: 'javascript:alert(1)' },
    { repo: 'https://example.com/repo' },
    { repo: 'https://github.com:8443/Thurbeen/thurbox-fixture', badge: 'community' },
    { repo: 'https://github.com/Thurbeen/thurbox-fixture.git' },
    { kind: 'wrong' },
    { badge: 'wrong' },
    { platforms: [] },
    { updated: '2026-02-30' },
    { media: { url: 'http://example.com/x.gif', alt: '' } },
  ]) {
    assert.ok(validatePlugins([{ ...fixture[0], ...patch }]).length, JSON.stringify(patch));
  }
  assert.ok(
    validatePlugins([
      fixture[0],
      { ...fixture[0], name: 'Another', repo: fixture[0].repo.toUpperCase() },
    ]).length,
  );
  assert.ok(validatePlugins([fixture[0], { ...fixture[1], name: fixture[0].name }]).length);
  assert.ok(validatePlugins({}).length);
  assert.ok(validatePlugins([null]).length);
});

test('link validation rejects a missing repository and unreachable media', async () => {
  const { checkLinks } = await import('./validate-plugins.mjs');
  const seen = [];
  const errors = await checkLinks(fixture, async (url) => {
    seen.push(url);
    return { ok: !url.includes('extension') && !url.endsWith('/demo.gif'), status: 404 };
  });
  assert.ok(seen.includes(fixture[1].repo));
  assert.ok(seen.includes(fixture[1].media.url));
  assert.ok(errors.some((e) => e.includes(fixture[1].repo)));
  assert.ok(errors.some((e) => e.includes(fixture[1].media.url)));
  const shipsDemo = await checkLinks(fixture, async () => ({ ok: true }));
  assert.ok(
    shipsDemo.some((e) => e.startsWith(fixture[0].repo) && e.includes('has no media')),
    'an entry without media is flagged when its repository ships media/demo.gif',
  );
  const noDemo = await checkLinks(fixture, async (url) => ({ ok: !url.endsWith('/demo.gif') }));
  assert.ok(!noDemo.some((e) => e.startsWith(fixture[0].repo)));
  const unavailable = await checkLinks(fixture, async () => {
    throw new Error('offline');
  });
  assert.ok(unavailable.length);
});

test('catalog inherits the website single theme', async () => {
  const css = await readFile('website/css/plugins.css', 'utf8');
  assert.doesNotMatch(css, /prefers-color-scheme|:root/);
});
