import { pathToFileURL } from 'node:url';
import { defineCatalog, nonEmptyString } from './gallery.mjs';

export const { TOPIC, SEARCH_QUERY, SNAPSHOT_PATH, REGISTRY_PATH, collectGallery, loadGallery, validateGallery, galleryData, main } =
  defineCatalog({
    noun: 'mods',
    topic: 'thurbox-mods',
    manifestFile: 'mod.toml',
    registryPath: 'website/_data/mods.json',
    snapshotPath: 'website/mods-snapshot.json',
    defaultMediaAlt: (repository) => `${repository.name} in the Thurbox interface`,
    defaultInstall: (repository) => `thurbox-cli mod install git+${repository.html_url}`,
    manifestFields: (manifest) => ({ requiresThurbox: nonEmptyString(manifest.requires_thurbox) }),
  });

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) await main();
