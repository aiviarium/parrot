import { docs } from 'collections/server';
import { loader } from 'fumadocs-core/source';
import { lucideIconsPlugin } from 'fumadocs-core/source/plugins/lucide-icons';

// `icon` in a folder's meta.json names a Lucide icon.
export const source = loader({
  baseUrl: '/docs',
  source: docs.toFumadocsSource(),
  plugins: [lucideIconsPlugin()],
  // docs/README.md is the first page, as GitHub shows it for the folder.
  slugs: (file, next) => (file.path === 'README.md' ? [] : next()),
});
