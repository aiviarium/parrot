import { defineConfig, defineDocs } from 'fumadocs-mdx/config';
import lastModified from 'fumadocs-mdx/plugins/last-modified';
import { pageSchema } from 'fumadocs-core/source/schema';
import { neutralDark, neutralLight } from './src/lib/shiki-themes';
import { firstHeading, remarkDocsLinks, remarkDropTitle } from './src/lib/markdown';

const LAYOUT_ONLY = new Set(['Steps', 'Step', 'Tabs', 'Tab']);

export const docs = defineDocs({
  // The pages are the Markdown files in the repository's docs/ folder, which
  // also read on GitHub. They have no front matter: the title is the first
  // heading.
  dir: '../docs',
  docs: {
    files: ['**/*.md'],
    schema: ({ source }) =>
      pageSchema.extend({ title: pageSchema.shape.title.default(firstHeading(source) ?? '') }),
    postprocess: {
      // Each page also as plain Markdown, for llms.txt and the .md files.
      includeProcessedMarkdown: {
        headingIds: false,
        // Layout components add nothing to the text; keep what is inside them.
        filterElement: (node) =>
          (node.type === 'mdxJsxFlowElement' || node.type === 'mdxJsxTextElement') &&
          node.name !== null &&
          LAYOUT_ONLY.has(node.name)
            ? 'children-only'
            : true,
      },
    },
  },
});

export default defineConfig({
  // "Last updated" on each page, from the git history.
  plugins: [lastModified()],
  mdxOptions: {
    // Before the defaults, so the title is not in the table of contents twice.
    remarkPlugins: (defaults) => [remarkDropTitle, remarkDocsLinks, ...defaults],
    rehypeCodeOptions: {
      themes: { light: neutralLight, dark: neutralDark },
      defaultColor: false,
    },
  },
});
