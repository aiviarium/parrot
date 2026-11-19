// The pages are the Markdown files in docs/, written to read on GitHub. These
// helpers turn them into site pages: the first heading becomes the title, and
// links between the files become links between pages.
import { dirname, join, relative, resolve, sep } from 'node:path';
import type { Link, Nodes, Root } from 'mdast';
import config from '../../docs.config';

/** The repository and its docs/ folder. The site is built from site/. */
const repoDir = resolve(process.cwd(), '..');
export const docsDir = join(repoDir, 'docs');

/** The text of the first `# heading`, outside code blocks. */
export function firstHeading(source: string): string | undefined {
  let fence = false;
  for (const line of source.split('\n')) {
    if (/^\s*(```|~~~)/.test(line)) fence = !fence;
    const heading = !fence && /^#\s+(.+?)\s*#*\s*$/.exec(line);
    if (heading) return heading[1].replace(/`/g, '');
  }
  return undefined;
}

/** docs/registries.md is the page /docs/registries; docs/README.md is /docs. */
export function fileToUrl(file: string): string | undefined {
  const rel = relative(docsDir, file).split(sep).join('/');
  if (rel.startsWith('..') || !rel.endsWith('.md')) return undefined;
  const slugs = rel.slice(0, -'.md'.length).split('/');
  const last = slugs[slugs.length - 1];
  if (last === 'README' || last === 'index') slugs.pop();
  return ['/docs', ...slugs].join('/');
}

/**
 * Where a link written in docs/ goes on the site. A link to another Markdown
 * file there goes to its page. A link to any other file in the repository,
 * such as ../examples/aviary, goes to that file on GitHub. Other links stay.
 */
export function siteHref(href: string, fromFile: string): string {
  if (/^[a-z][a-z0-9+.-]*:/i.test(href) || href.startsWith('#') || href.startsWith('/')) {
    return href;
  }
  const [path, hash] = href.split('#', 2);
  const target = resolve(dirname(fromFile), decodeURI(path));
  const page = fileToUrl(target);
  const suffix = hash === undefined ? '' : `#${hash}`;
  if (page) return `${page}${suffix}`;
  const inRepo = relative(repoDir, target).split(sep).join('/');
  if (inRepo.startsWith('..')) return href;
  return `${config.repo}/tree/main/${inRepo}${suffix}`;
}

function visitLinks(node: Nodes, fn: (link: Link) => void) {
  if (node.type === 'link' || node.type === 'definition') fn(node as Link);
  if ('children' in node) for (const child of node.children) visitLinks(child, fn);
}

/** Removes the first `# heading`: the page shows the title above the body. */
export function remarkDropTitle() {
  return (tree: Root) => {
    const index = tree.children.findIndex((n) => n.type === 'heading' && n.depth === 1);
    if (index !== -1) tree.children.splice(index, 1);
  };
}

/** Rewrites links between the Markdown files to links between pages. */
export function remarkDocsLinks() {
  return (tree: Root, file: { path?: string }) => {
    const from = file.path;
    if (!from) return;
    visitLinks(tree, (link) => {
      link.url = siteHref(link.url, from);
    });
  };
}
