import { pathToFileURL } from 'node:url';
import { defineCatalog, nonEmptyString } from './gallery.mjs';

const KIND_NAME = /^[A-Z][A-Za-z0-9]*$/;

const kindsOf = (value) =>
  Array.isArray(value) ? value.filter((kind) => typeof kind === 'string' && KIND_NAME.test(kind)) : [];

export const { TOPIC, SEARCH_QUERY, SNAPSHOT_PATH, REGISTRY_PATH, collectGallery, loadGallery, validateGallery, galleryData, main } =
  defineCatalog({
    noun: 'Fleet plugins',
    topic: 'thurbox-fleet-plugins',
    manifestFile: 'fleet-plugin.toml',
    registryPath: 'website/_data/fleet-plugins.json',
    snapshotPath: 'website/fleet-plugins-snapshot.json',
    defaultMediaAlt: (repository) => `${repository.name} preview`,
    defaultInstall: (repository) =>
      `git clone ${repository.html_url} /opt/fleet-plugins/${repository.name}`,
    manifestFields: (manifest) => ({
      requiresFleet: nonEmptyString(manifest.requires_fleet),
      kinds: kindsOf(manifest.kinds),
    }),
    validateItem: (plugin) =>
      Array.isArray(plugin.kinds) && plugin.kinds.every((kind) => KIND_NAME.test(kind))
        ? []
        : ['kinds must be a list of CamelCase kind names'],
  });

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) await main();
