import { fileURLToPath } from 'node:url';
import { createMDX } from 'fumadocs-mdx/next';

const withMDX = createMDX();

// Set when the site is served from a folder, such as /parrot on github.io.
const basePath = process.env.DOCS_BASE_PATH || undefined;

/** @type {import('next').NextConfig} */
const config = {
  // A static site: `npm run build` writes it to out/.
  output: 'export',
  trailingSlash: true,
  reactStrictMode: true,
  basePath,
  // The pages are in ../docs, outside this folder, so the bundler starts at the
  // root of the repository. The compiled pages import fumadocs-core/server,
  // which is not found from docs/, so it is named here.
  turbopack: {
    root: fileURLToPath(new URL('..', import.meta.url)),
    resolveAlias: {
      'fumadocs-core/server': {
        browser: './node_modules/fumadocs-core/dist/server.browser.js',
        default: './node_modules/fumadocs-core/dist/server.js',
      },
    },
  },
  // A static host has no image server.
  images: { unoptimized: true },
  // Code that names files in public/ or fetches the search index adds the folder itself.
  env: { NEXT_PUBLIC_BASE_PATH: basePath ?? '' },
};

export default withMDX(config);
