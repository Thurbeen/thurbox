import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';

export const schema = JSON.parse(
  await readFile(new URL('../../website/mods.schema.json', import.meta.url), 'utf8'),
);

function validUrl(value, repository = false) {
  try {
    const url = new URL(value);
    return (
      url.protocol === 'https:' &&
      !url.username &&
      !url.password &&
      (!repository ||
        (url.hostname === 'github.com' &&
          new RegExp(schema.items.properties.repo.pattern).test(value) &&
          !url.search &&
          !url.hash))
    );
  } catch {
    return false;
  }
}

const nonEmpty = (value) => typeof value === 'string' && value.trim() !== '';

export function validateOverlay(entries) {
  if (!Array.isArray(entries)) return ['Registry must be an array'];
  const errors = [];
  const repos = new Set();
  for (const [i, entry] of entries.entries()) {
    const fail = (message) => errors.push(`Entry ${i + 1}: ${message}`);
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
      fail('must be an object');
      continue;
    }
    for (const field of Object.keys(entry))
      if (!Object.hasOwn(schema.items.properties, field)) fail(`unknown field ${field}`);
    if (!validUrl(entry.repo, true)) fail('repo must be a canonical HTTPS GitHub repository URL');
    if (entry.hidden !== undefined && typeof entry.hidden !== 'boolean')
      fail('hidden must be a boolean');
    for (const field of ['install', 'notes'])
      if (entry[field] !== undefined && !nonEmpty(entry[field]))
        fail(`${field} must be a nonempty string`);
    if (
      entry.platforms !== undefined &&
      (!Array.isArray(entry.platforms) ||
        !entry.platforms.length ||
        !entry.platforms.every(nonEmpty) ||
        new Set(entry.platforms).size !== entry.platforms.length)
    )
      fail('platforms must be distinct nonempty strings');
    if (
      entry.media !== undefined &&
      (!entry.media ||
        !validUrl(entry.media.url) ||
        !nonEmpty(entry.media.alt) ||
        Object.keys(entry.media).some((k) => !['url', 'alt'].includes(k)))
    )
      fail('media needs an HTTPS url and nonempty alt');
    const repo = typeof entry.repo === 'string' ? entry.repo.toLowerCase() : '';
    if (repos.has(repo)) fail('duplicate repo');
    repos.add(repo);
  }
  return errors;
}

export async function checkLinks(entries, request = fetch) {
  const errors = [];
  const urls = new Set(
    entries
      .filter((entry) => !entry.hidden)
      .flatMap((entry) => [entry.repo, ...(entry.media ? [entry.media.url] : [])]),
  );
  for (const url of urls) {
    try {
      const response = await request(url, {
        method: 'HEAD',
        redirect: 'follow',
        signal: AbortSignal.timeout(15000),
      });
      if (!response.ok) errors.push(`${url}: HTTP ${response.status}`);
    } catch (error) {
      errors.push(`${url}: ${error.message}`);
    }
  }
  return errors;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const entries = JSON.parse(
    await readFile(process.argv[2] || 'website/_data/mods.json', 'utf8'),
  );
  let errors = validateOverlay(entries);
  if (!errors.length && process.argv.includes('--links')) errors = await checkLinks(entries);
  if (errors.length) {
    console.error(errors.join('\n'));
    process.exitCode = 1;
  } else
    console.log(
      `Validated ${entries.length} registry entries${process.argv.includes('--links') ? ' and their URLs' : ''}`,
    );
}
