import { readFile, writeFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
import { parse as parseToml } from 'smol-toml';
import { validateMods } from './validate-mods.mjs';

export const TOPIC = 'thurbox-mods';
export const SEARCH_QUERY = `topic:${TOPIC} is:public fork:false archived:false`;
export const SNAPSHOT_PATH = 'website/mods-snapshot.json';
export const REGISTRY_PATH = 'website/_data/mods.json';

const API = 'https://api.github.com';
const RAW = 'https://raw.githubusercontent.com';
const SEARCH_PAGE_SIZE = 100;
const SEARCH_RESULT_CAP = 1000;
const REQUEST_TIMEOUT_MS = 15000;
const OFFICIAL_OWNER = 'Thurbeen';
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

async function searchByTopic(request, token) {
  const found = [];
  for (let page = 1; found.length < SEARCH_RESULT_CAP; page += 1) {
    const params = new URLSearchParams({
      q: SEARCH_QUERY,
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

const nonEmptyString = (value) => (typeof value === 'string' && value.trim() ? value : null);

function stringList(value) {
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

async function describe(request, repository, curatedEntry, tagged) {
  const fullName = repository.full_name;
  const base = `${RAW}/${fullName}/${repository.default_branch}`;
  const manifest = readManifest(await raw(request, `${base}/mod.toml`));
  const listing = manifest.gallery && typeof manifest.gallery === 'object' ? manifest.gallery : {};
  const curatedMedia = curatedEntry.media ?? null;
  let media = curatedMedia;
  if (!media) {
    const url = mediaUrl(listing.media, base);
    if (url)
      media = { url, alt: nonEmptyString(listing.media_alt) ?? `${repository.name} preview` };
  }
  if (!media && (await raw(request, `${base}/${DEFAULT_MEDIA_PATH}`, 'HEAD')) !== null)
    media = {
      url: `${base}/${DEFAULT_MEDIA_PATH}`,
      alt: `${repository.name} in the Thurbox interface`,
    };
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
    topics: (repository.topics ?? []).filter((topic) => topic !== TOPIC),
    official: repository.owner.login === OFFICIAL_OWNER,
    tagged,
    version: nonEmptyString(manifest.version),
    requiresThurbox: nonEmptyString(manifest.requires_thurbox),
    install:
      nonEmptyString(curatedEntry.install) ??
      (repository.owner.login === OFFICIAL_OWNER ? nonEmptyString(listing.install) : null) ??
      `thurbox-cli mod install git+${repository.html_url}`,
    platforms: curatedEntry.platforms ?? stringList(listing.platforms) ?? [],
    notes: nonEmptyString(curatedEntry.notes) ?? nonEmptyString(listing.notes),
    media,
  };
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

export async function collectGallery({ curated, request = fetch, token }) {
  const overlay = overlayByName(curated);
  const hidden = (fullName) => overlay.get(fullName.toLowerCase())?.hidden === true;
  const repositories = new Map();
  for (const repository of await searchByTopic(request, token)) {
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

function withOverlay(snapshot, curated) {
  const overlay = overlayByName(curated);
  return snapshot.flatMap((mod) => {
    const entry = overlay.get(mod.fullName.toLowerCase());
    if (!entry) return [mod];
    if (entry.hidden) return [];
    return [
      {
        ...mod,
        install: entry.install ?? mod.install,
        platforms: entry.platforms ?? mod.platforms,
        notes: entry.notes ?? mod.notes,
        media: entry.media ?? mod.media,
      },
    ];
  });
}

export async function loadGallery({ curated, snapshot, request = fetch, env, warn }) {
  if (env.THURBOX_MODS_OFFLINE) return withOverlay(snapshot, curated);
  try {
    return await collectGallery({
      curated,
      request,
      token: env.GITHUB_TOKEN || env.GH_TOKEN,
    });
  } catch (error) {
    warn(`[mods] GitHub discovery failed (${error.message}); using ${SNAPSHOT_PATH}`);
    return withOverlay(snapshot, curated);
  }
}

export function validateGallery(gallery) {
  if (!Array.isArray(gallery)) return ['Gallery must be an array'];
  const errors = [];
  for (const [i, mod] of gallery.entries()) {
    const fail = (message) => errors.push(`Mod ${i + 1}: ${message}`);
    for (const field of ['repo', 'fullName', 'owner', 'name', 'description', 'install'])
      if (!nonEmptyString(mod?.[field])) fail(`${field} must be a nonempty string`);
    if (typeof mod?.repo === 'string' && mod.repo !== `https://github.com/${mod.fullName}`)
      fail('repo must be the repository URL');
    if (!Number.isInteger(mod?.stars) || mod.stars < 0) fail('stars must be a count');
    for (const field of ['pushedAt', 'createdAt'])
      if (!Number.isFinite(Date.parse(mod?.[field]))) fail(`${field} must be a timestamp`);
    if (!Array.isArray(mod?.topics) || !Array.isArray(mod?.platforms))
      fail('topics and platforms must be lists');
    if (typeof mod?.official !== 'boolean' || typeof mod?.tagged !== 'boolean')
      fail('official and tagged must be booleans');
    if (mod?.official !== (mod?.owner === OFFICIAL_OWNER)) fail(`official means ${OFFICIAL_OWNER}`);
    if (mod?.media !== null && !/^https:\/\//.test(mod?.media?.url ?? ''))
      fail('media needs an HTTPS url');
  }
  return errors;
}

async function readJson(path) {
  return JSON.parse(await readFile(path, 'utf8'));
}

export async function galleryData({ request = fetch, env = process.env, warn = console.warn } = {}) {
  return loadGallery({
    curated: await readJson(REGISTRY_PATH),
    snapshot: await readJson(SNAPSHOT_PATH),
    request,
    env,
    warn,
  });
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const curated = await readJson(REGISTRY_PATH);
  const registryErrors = validateMods(curated);
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
  if (process.argv.includes('--write')) {
    await writeFile(SNAPSHOT_PATH, `${JSON.stringify(gallery, null, 2)}\n`);
    console.log(`Wrote ${gallery.length} mods to ${SNAPSHOT_PATH}`);
  } else console.log(`Discovered ${gallery.length} mods`);
}
