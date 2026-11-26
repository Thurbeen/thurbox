import { readFile, writeFile } from 'node:fs/promises';
import { parse as parseToml } from 'smol-toml';
import { validateOverlay } from './validate-mods.mjs';

export const OFFLINE_ENV = 'THURBOX_GALLERY_OFFLINE';

const API = 'https://api.github.com';
const RAW = 'https://raw.githubusercontent.com';
const SEARCH_PAGE_SIZE = 100;
const SEARCH_RESULT_CAP = 1000;
const REQUEST_TIMEOUT_MS = 15000;
export const OFFICIAL_OWNER = 'Thurbeen';
const DEFAULT_MEDIA_PATH = 'media/demo.gif';
const CONCURRENT_REPOSITORIES = 8;

class FetchError extends Error {}

function apiHeaders(token) {
  return {
    Accept: 'application/vnd.github+json',
    'X-GitHub-Api-Version': '2022-11-28',
    'User-Agent': 'thurbox-website',
    ...(token ? { Authorization: `Bearer ${token}` } : {}),
  };
}

async function api(request, token, path) {
  const response = await request(`${API}${path}`, {
    headers: apiHeaders(token),
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
  });
  if (response.status === 404) return null;
  if (!response.ok) throw new FetchError(`GET ${path}: HTTP ${response.status}`);
  return response.json();
}

async function raw(request, url, method = 'GET') {
  const response = await request(url, {
    method,
    headers: { 'User-Agent': 'thurbox-website' },
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
  });
  if (response.status === 404) return null;
  if (!response.ok) throw new FetchError(`${method} ${url}: HTTP ${response.status}`);
  return method === 'HEAD' ? '' : response.text();
}

async function searchByTopic(request, token, query) {
  const found = [];
  for (let page = 1; found.length < SEARCH_RESULT_CAP; page += 1) {
    const params = new URLSearchParams({
      q: query,
      sort: 'stars',
      per_page: String(SEARCH_PAGE_SIZE),
      page: String(page),
    });
    const body = await api(request, token, `/search/repositories?${params}`);
    if (!body) throw new FetchError('repository search returned 404');
    if (body.incomplete_results) throw new FetchError('repository search timed out on GitHub');
    found.push(...body.items);
    if (body.items.length < SEARCH_PAGE_SIZE || found.length >= body.total_count) break;
  }
  return found;
}

function fullNameOf(repoUrl) {
  return new URL(repoUrl).pathname.slice(1);
}

function readManifest(text) {
  if (!text) return {};
  try {
    const manifest = parseToml(text);
    return manifest && typeof manifest === 'object' ? manifest : {};
  } catch {
    return {};
  }
}

export const nonEmptyString = (value) =>
  typeof value === 'string' && value.trim() ? value : null;

export function stringList(value) {
  return Array.isArray(value) ? value.filter((item) => nonEmptyString(item)) : null;
}

function mediaUrl(value, base) {
  const candidate = nonEmptyString(value);
  if (!candidate) return null;
  if (/^https:\/\//.test(candidate)) return candidate;
  if (/^[a-z][a-z0-9+.-]*:/i.test(candidate) || candidate.startsWith('/')) return null;
  if (candidate.split('/').some((segment) => segment === '..' || segment === '')) return null;
  return `${base}/${candidate}`;
}

async function mapConcurrently(items, limit, work) {
  const results = new Array(items.length);
  let next = 0;
  const worker = async () => {
    while (next < items.length) {
      const index = next;
      next += 1;
      results[index] = await work(items[index]);
    }
  };
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, worker));
  return results;
}

function byPopularity(a, b) {
  return (
    b.stars - a.stars ||
    Date.parse(b.pushedAt) - Date.parse(a.pushedAt) ||
    a.fullName.localeCompare(b.fullName)
  );
}

function overlayByName(curated) {
  return new Map(curated.map((entry) => [fullNameOf(entry.repo).toLowerCase(), entry]));
}

function withOverlay(snapshot, curated) {
  const overlay = overlayByName(curated);
  return snapshot.flatMap((item) => {
    const entry = overlay.get(item.fullName.toLowerCase());
    if (!entry) return [item];
    if (entry.hidden) return [];
    return [
      {
        ...item,
        install: entry.install ?? item.install,
        platforms: entry.platforms ?? item.platforms,
        notes: entry.notes ?? item.notes,
        media: entry.media ?? item.media,
      },
    ];
  });
}

async function readJson(path) {
  return JSON.parse(await readFile(path, 'utf8'));
}

export function defineCatalog({
  noun,
  topic,
  manifestFile,
  registryPath,
  snapshotPath,
  defaultMediaAlt,
  defaultInstall,
  manifestFields,
  validateItem = () => [],
}) {
  const searchQuery = `topic:${topic} is:public fork:false archived:false`;

  async function describe(request, repository, curatedEntry, tagged) {
    const fullName = repository.full_name;
    const base = `${RAW}/${fullName}/${repository.default_branch}`;
    const manifest = readManifest(await raw(request, `${base}/${manifestFile}`));
    const listing =
      manifest.gallery && typeof manifest.gallery === 'object' ? manifest.gallery : {};
    const official = repository.owner.login === OFFICIAL_OWNER;
    let media = curatedEntry.media ?? null;
    if (!media) {
      const url = mediaUrl(listing.media, base);
      if (url)
        media = { url, alt: nonEmptyString(listing.media_alt) ?? `${repository.name} preview` };
    }
    if (!media && (await raw(request, `${base}/${DEFAULT_MEDIA_PATH}`, 'HEAD')) !== null)
      media = { url: `${base}/${DEFAULT_MEDIA_PATH}`, alt: defaultMediaAlt(repository) };
    return {
      repo: repository.html_url,
      fullName,
      owner: repository.owner.login,
      name: repository.name,
      description:
        nonEmptyString(repository.description) ??
        nonEmptyString(manifest.description) ??
        'No description provided.',
      stars: repository.stargazers_count ?? 0,
      pushedAt: repository.pushed_at ?? repository.created_at,
      createdAt: repository.created_at,
      topics: (repository.topics ?? []).filter((t) => t !== topic),
      official,
      tagged,
      version: nonEmptyString(manifest.version),
      ...manifestFields(manifest),
      install:
        nonEmptyString(curatedEntry.install) ??
        (official ? nonEmptyString(listing.install) : null) ??
        defaultInstall(repository),
      platforms: curatedEntry.platforms ?? stringList(listing.platforms) ?? [],
      notes: nonEmptyString(curatedEntry.notes) ?? nonEmptyString(listing.notes),
      media,
    };
  }

  async function collectGallery({ curated, request = fetch, token }) {
    const overlay = overlayByName(curated);
    const hidden = (fullName) => overlay.get(fullName.toLowerCase())?.hidden === true;
    const repositories = new Map();
    for (const repository of await searchByTopic(request, token, searchQuery)) {
      if (repository.fork || repository.archived || repository.private) continue;
      if (hidden(repository.full_name)) continue;
      repositories.set(repository.full_name.toLowerCase(), { repository, tagged: true });
    }
    for (const [key, entry] of [...overlay]) {
      if (entry.hidden || repositories.has(key)) continue;
      const repository = await api(request, token, `/repos/${fullNameOf(entry.repo)}`);
      if (!repository || repository.archived || repository.private) continue;
      const currentName = repository.full_name.toLowerCase();
      if (!overlay.has(currentName)) overlay.set(currentName, entry);
      if (!repositories.has(currentName))
        repositories.set(currentName, { repository, tagged: false });
    }
    const gallery = await mapConcurrently(
      [...repositories.entries()],
      CONCURRENT_REPOSITORIES,
      ([key, { repository, tagged }]) =>
        describe(request, repository, overlay.get(key) ?? {}, tagged),
    );
    return gallery.sort(byPopularity);
  }

  async function loadGallery({ curated, snapshot, request = fetch, env, warn }) {
    if (env[OFFLINE_ENV]) return withOverlay(snapshot, curated);
    try {
      return await collectGallery({ curated, request, token: env.GITHUB_TOKEN || env.GH_TOKEN });
    } catch (error) {
      warn(`[${noun}] GitHub discovery failed (${error.message}); using ${snapshotPath}`);
      return withOverlay(snapshot, curated);
    }
  }

  function validateGallery(gallery) {
    if (!Array.isArray(gallery)) return ['Gallery must be an array'];
    const errors = [];
    for (const [i, item] of gallery.entries()) {
      const fail = (message) => errors.push(`Entry ${i + 1}: ${message}`);
      for (const field of ['repo', 'fullName', 'owner', 'name', 'description', 'install'])
        if (!nonEmptyString(item?.[field])) fail(`${field} must be a nonempty string`);
      if (typeof item?.repo === 'string' && item.repo !== `https://github.com/${item.fullName}`)
        fail('repo must be the repository URL');
      if (!Number.isInteger(item?.stars) || item.stars < 0) fail('stars must be a count');
      for (const field of ['pushedAt', 'createdAt'])
        if (!Number.isFinite(Date.parse(item?.[field]))) fail(`${field} must be a timestamp`);
      if (!Array.isArray(item?.topics) || !Array.isArray(item?.platforms))
        fail('topics and platforms must be lists');
      if (typeof item?.official !== 'boolean' || typeof item?.tagged !== 'boolean')
        fail('official and tagged must be booleans');
      if (item?.official !== (item?.owner === OFFICIAL_OWNER))
        fail(`official means ${OFFICIAL_OWNER}`);
      if (item?.media !== null && !/^https:\/\//.test(item?.media?.url ?? ''))
        fail('media needs an HTTPS url');
      for (const message of validateItem(item ?? {})) fail(message);
    }
    return errors;
  }

  async function galleryData({ request = fetch, env = process.env, warn = console.warn } = {}) {
    return loadGallery({
      curated: await readJson(registryPath),
      snapshot: await readJson(snapshotPath),
      request,
      env,
      warn,
    });
  }

  async function main(argv = process.argv) {
    const curated = await readJson(registryPath);
    const registryErrors = validateOverlay(curated);
    if (registryErrors.length) {
      console.error(registryErrors.join('\n'));
      process.exit(1);
    }
    const gallery = await collectGallery({
      curated,
      token: process.env.GITHUB_TOKEN || process.env.GH_TOKEN,
    });
    const errors = validateGallery(gallery);
    if (errors.length) {
      console.error(errors.join('\n'));
      process.exit(1);
    }
    if (argv.includes('--write')) {
      await writeFile(snapshotPath, `${JSON.stringify(gallery, null, 2)}\n`);
      console.log(`Wrote ${gallery.length} ${noun} to ${snapshotPath}`);
    } else console.log(`Discovered ${gallery.length} ${noun}`);
  }

  return {
    TOPIC: topic,
    SEARCH_QUERY: searchQuery,
    SNAPSHOT_PATH: snapshotPath,
    REGISTRY_PATH: registryPath,
    collectGallery,
    loadGallery,
    validateGallery,
    galleryData,
    main,
  };
}
