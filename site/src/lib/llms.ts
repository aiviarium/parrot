import { resolve } from 'node:path';
import { llms } from 'fumadocs-core/source';
import type { InferPageType } from 'fumadocs-core/source';
import { basePath, config } from '@/lib/site';
import { docsDir, siteHref } from '@/lib/markdown';
import { source } from '@/lib/source';

type DocPage = InferPageType<typeof source>;

/** Where a page is served as Markdown: /md/components/tabs.md, or /md/index.md for the first page. */
export function markdownSlug(page: DocPage): string[] {
  const slugs = page.slugs.length ? page.slugs : ['index'];
  return [...slugs.slice(0, -1), `${slugs[slugs.length - 1]}.md`];
}

export function markdownUrl(page: DocPage): string {
  return `${basePath}/md/${markdownSlug(page).join('/')}`;
}

/**
 * Tidies the Markdown that removing layout components leaves behind: a heading
 * or a fence that lost the blank line before it gets one back. Lines inside
 * code fences are left alone.
 */
function tidy(text: string): string {
  const out: string[] = [];
  let fence = false;
  for (const line of text.split('\n')) {
    const isFence = /^\s*(```|~~~)/.test(line);
    const opensBlock = !fence && (isFence || /^#{1,6} /.test(line));
    if (opensBlock && out.length > 0 && out[out.length - 1].trim() !== '') out.push('');
    if (isFence) fence = !fence;
    out.push(line);
  }
  return out.join('\n');
}

/**
 * Links written as file names, such as concepts.md, point to the Markdown file
 * of that page. Links to other files in the repository point to GitHub.
 */
function rewriteLinks(text: string, page: DocPage): string {
  const from = resolve(docsDir, page.path);
  return text.replace(/\]\(([^)\s]+)\)/g, (match, href: string) => {
    const url = siteHref(href, from);
    if (url === href) return match;
    const [path, hash] = url.split('#', 2);
    const target = source.getPages().find((candidate) => candidate.url === path);
    if (!target) return `](${url})`;
    return `](${markdownUrl(target)}${hash === undefined ? '' : `#${hash}`})`;
  });
}

/** The page's own `# title`, which pageMarkdown writes itself. */
const dropTitle = (text: string) => text.replace(/^\s*# [^\n]*\n/, '');

/** A page as plain Markdown, for programs that read the docs. */
export async function pageMarkdown(page: DocPage): Promise<string> {
  const text = rewriteLinks(dropTitle(tidy(await page.data.getText('processed'))), page);
  const about = page.data.description ? `\n\n${page.data.description}` : '';
  return `# ${page.data.title}${about}\n\nSource: ${basePath}${page.url}/\n\n${text.trim()}\n`;
}

// llms.txt links each page to its Markdown file, so a reader that follows a link gets Markdown.
export const llm = llms(source, {
  renderPage: pageMarkdown,
  renderUrl: (item) => {
    const page = source.getPages().find((candidate) => candidate.url === item.url);
    return page ? markdownUrl(page) : `${basePath}${item.url}`;
  },
});

/** llms.txt: the site's name and description, then every page with a link to its Markdown. */
export async function llmsIndex(): Promise<string> {
  const tree = (await llm.index()).replace(/^# .*\n+(> .*\n+)?/, '');
  return `# ${config.name}\n\n> ${config.description}\n\nAll pages in one file: ${basePath}/llms-full.txt\n\n${tree.trim()}\n`;
}
