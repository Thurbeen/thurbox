import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';

export const schema = JSON.parse(
  await readFile(new URL('../../website/plugins.schema.json', import.meta.url), 'utf8'),
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

export function validatePlugins(entries) {
  if (!Array.isArray(entries)) return ['Catalog must be an array'];
  const errors = [];
  const repos = new Set();
  const names = new Set();
  for (const [i, entry] of entries.entries()) {
    const fail = (message) => errors.push(`Entry ${i + 1}: ${message}`);
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
      fail('must be an object');
      continue;
    }
    for (const field of schema.items.required) {
      if (field === 'platforms') {
        if (
          !Array.isArray(entry.platforms) ||
          !entry.platforms.length ||
          entry.platforms.some((p) => typeof p !== 'string' || !p.trim()) ||
          new Set(entry.platforms).size !== entry.platforms.length
        )
          fail('platforms must be distinct nonempty strings');
      } else if (typeof entry[field] !== 'string' || !entry[field].trim())
        fail(`${field} must be a nonempty string`);
    }
    for (const field of Object.keys(entry))
      if (!Object.hasOwn(schema.items.properties, field)) fail(`unknown field ${field}`);
    for (const field of ['kind', 'badge'])
      if (!schema.items.properties[field].enum.includes(entry[field])) fail(`invalid ${field}`);
    if (!validUrl(entry.repo, true)) fail('repo must be a canonical HTTPS GitHub repository URL');
    if (
      typeof entry.updated !== 'string' ||
      !/^\d{4}-\d{2}-\d{2}$/.test(entry.updated) ||
      !Number.isFinite(Date.parse(entry.updated)) ||
      new Date(entry.updated).toISOString().slice(0, 10) !== entry.updated
    )
      fail('updated must be a real ISO date');
    if (
      entry.media !== undefined &&
      (!entry.media ||
        !validUrl(entry.media.url) ||
        typeof entry.media.alt !== 'string' ||
        !entry.media.alt.trim() ||
        Object.keys(entry.media).some((k) => !['url', 'alt'].includes(k)))
    )
      fail('media needs an HTTPS url and nonempty alt');
    if (entry.release !== undefined && (typeof entry.release !== 'string' || !entry.release.trim()))
      fail('release must be a nonempty string');
    for (const [field, seen] of [
      ['repo', repos],
      ['name', names],
    ]) {
      const value = typeof entry[field] === 'string' ? entry[field].toLowerCase() : '';
      if (seen.has(value)) fail(`duplicate ${field}`);
      seen.add(value);
    }
    if (
      entry.badge === 'official' &&
      typeof entry.repo === 'string' &&
      !entry.repo.startsWith('https://github.com/Thurbeen/')
    )
      fail('official entries must belong to Thurbeen');
  }
  return errors;
}

export async function checkLinks(entries, request = fetch) {
  const errors = [];
  const urls = new Set(
    entries.flatMap((entry) => [entry.repo, ...(entry.media ? [entry.media.url] : [])]),
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
  // Every catalog GIF sits at the same repository path, so a missing preview
  // is caught when an author adds one after the entry was written.
  for (const entry of entries.filter((e) => !e.media)) {
    const demo = `${entry.repo.replace('https://github.com/', 'https://raw.githubusercontent.com/')}/HEAD/media/demo.gif`;
    try {
      const response = await request(demo, {
        method: 'HEAD',
        redirect: 'follow',
        signal: AbortSignal.timeout(15000),
      });
      if (response.ok) errors.push(`${entry.repo}: ships ${demo} but the entry has no media`);
    } catch {
      // An unreachable optional preview is not an error; the repo check above reports outages.
    }
  }
  return errors;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const entries = JSON.parse(
    await readFile(process.argv[2] || 'website/_data/plugins.json', 'utf8'),
  );
  let errors = validatePlugins(entries);
  if (!errors.length && process.argv.includes('--links')) errors = await checkLinks(entries);
  if (errors.length) {
    console.error(errors.join('\n'));
    process.exitCode = 1;
  } else
    console.log(
      `Validated ${entries.length} plugins${process.argv.includes('--links') ? ' and their URLs' : ''}`,
    );
}
