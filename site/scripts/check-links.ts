// Checks the links between pages: every internal link, every link written as a
// file name and every #heading must point to something the site builds. The
// links in docs.config.ts are checked too. External links are not fetched.
//
// The pages are the Markdown files in ../docs. Their links are checked as the
// site rewrites them, and a link to another file in the repository has to name
// a file or folder that exists.
//
// Run with `npm run links`. It exits with 1 when a link is broken.
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import GithubSlugger from 'github-slugger';
import { printErrors, validateFiles, type ScanResult } from 'next-validate-link';
import config from '../docs.config';
import { docsDir, fileToUrl, siteHref } from '../src/lib/markdown';


function walk(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return walk(path);
    return /\.mdx?$/.test(entry.name) ? [path] : [];
  });
}

const pathToUrl = (file: string) => fileToUrl(resolve(file));

const repoTree = `${config.repo}/tree/main/`;
const missingFiles: string[] = [];

/** The links of a page as the site has them. Code blocks are left alone. */
function rewrite(path: string, content: string): string {
  let fence = false;
  return content
    .split('\n')
    .map((line) => {
      if (/^\s*(```|~~~)/.test(line)) fence = !fence;
      if (fence) return line;
      return line.replace(/\]\(([^)\s]+)\)/g, (_, href: string) => {
        const url = siteHref(href, resolve(path));
        if (url !== href && url.startsWith(repoTree)) {
          const file = resolve(docsDir, '..', url.slice(repoTree.length).split('#')[0]);
          if (!existsSync(file)) missingFiles.push(`${path}: ${href}`);
        }
        return `](${url})`;
      });
    })
    .join('\n');
}

/** Heading ids as Fumadocs makes them: github-slugger, or an explicit [#id] after the text. */
function headingIds(content: string): string[] {
  const slugger = new GithubSlugger();
  const ids: string[] = [];
  let fence = false;
  for (const line of content.split('\n')) {
    if (/^\s*(```|~~~)/.test(line)) fence = !fence;
    const heading = !fence && /^#{1,6}\s+(.+?)\s*$/.exec(line);
    if (!heading) continue;
    const explicit = /\[#([^\]]+)\]$/.exec(heading[1]);
    const text = heading[1].replace(/`/g, '').replace(/\[([^\]]*)\]\([^)]*\)/g, '$1');
    ids.push(explicit ? explicit[1] : slugger.slug(text));
  }
  return ids;
}

const files = walk(docsDir).map((path) => {
  const content = rewrite(path, readFileSync(path, 'utf8'));
  return { path, content, url: pathToUrl(path) };
});

const scanned: ScanResult = { urls: new Map(), fallbackUrls: [] };
for (const file of files) {
  if (!file.url) continue;
  scanned.urls.set(file.url, { hashes: headingIds(file.content) });
}
for (const url of ['/', '/llms.txt', '/llms-full.txt']) scanned.urls.set(url, {});
for (const file of files) {
  if (file.url) scanned.urls.set(`/md${file.url.slice('/docs'.length) || '/index'}.md`, {});
}

const results = await validateFiles(files, {
  scanned,
  pathToUrl,
  checkRelativePaths: 'as-url',
  markdown: { components: { Card: { attributes: ['href'] } } },
});

// The links in docs.config.ts, which the pages do not contain.
const configLinks = [
  ...config.nav.map((link) => link.url),
  ...config.actions.map((link) => link.url),
  ...config.features.flatMap((feature) => (feature.url ? [feature.url] : [])),
];
const configErrors = configLinks
  .filter((url) => url.startsWith('/'))
  .filter((url) => {
    const [path, hash] = url.replace(/\/(?=#|$)/, '').split('#');
    const meta = scanned.urls.get(path || '/');
    return !meta || (hash !== undefined && !meta.hashes?.includes(hash));
  });

printErrors(results);
for (const url of configErrors) console.error(`docs.config.ts: ${url}: not-found`);
for (const link of missingFiles) console.error(`${link}: no such file in the repository`);

const broken =
  results.reduce((count, result) => count + result.errors.length, 0) +
  configErrors.length +
  missingFiles.length;
if (broken > 0) {
  console.error(`${broken} broken link(s).`);
  process.exit(1);
}
console.log(`Checked ${files.length} pages: no broken links.`);
