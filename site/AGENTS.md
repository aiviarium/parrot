# Working on the documentation site

This folder is the documentation site of Parrot, served at <https://parrot.helight.dev/>. It is a static Fumadocs site on Next.js and follows [aiviarium/docs-base](https://github.com/aiviarium/docs-base). This file is for coding agents working in it.

## Layout

| Path | What it is |
| --- | --- |
| `../docs/` | The pages. They are the Markdown files of the repository, and `../docs/meta.json` gives their order |
| `docs.config.ts` | The name, colour, links, badges and assets of Parrot |
| `public/brand/` | The icons and wordmarks of Parrot |
| `source.config.ts` | The content source, which reads `../docs` |
| `src/lib/markdown.ts` | Turns the Markdown files into pages, with their titles and links |
| `src/app/` | The routes of the landing page, docs, search index, `llms.txt`, `llms-full.txt` and `/md/*.md` |
| `src/app/global.css` | The theme. See `docs/design.md` |
| `src/components/` | The lockup, wordmark, badge and MDX components |
| `scripts/check-links.ts` | The link check |
| `docs/design.md` | The design rules |

## Pages

The pages are the files in `../docs`, so they read the same on GitHub and on the site.

- A page has no front matter. Its title is its first `# heading`, which the site shows above the page and leaves out of the body.
- `README.md` is the first page, at `/docs`.
- Link to another page by its file name, as in `[Registries](drivers/claude.md#options)`. The site turns it into a link to the page. A link to any other file in the repository, such as `../spec/protocol.md`, goes to that file on GitHub.
- A new page goes into the `meta.json` of its folder, or it is listed after the others.
- The files are Markdown, not MDX. Components from the docs base are not available in them.
- Every claim has to be true of the code in `../crates` and `../packages`. Name commands, flags and paths as the source does.

## Build and check

```bash
npm install
npm run dev         # http://localhost:3000
npm run typecheck
npm run lint
npm run links
npm run build       # writes out/; DOCS_BASE_PATH=/parrot for GitHub Pages
```

`npm run check` runs the type check, the lint, the link check and the build. Vale runs from the root of the repository in CI.

## Deploy

Pages serves the docs at the root and the egg registry under `/eggs`. Two workflows publish the whole site:

- [`../.github/workflows/docs.yml`](../.github/workflows/docs.yml) builds the docs and adds the newest registry from the latest release.
- [`../.github/workflows/release.yml`](../.github/workflows/release.yml) builds the docs beside the registry it just packed.

The custom domain, `parrot.helight.dev`, is set in the repository's Pages settings. There is no `CNAME` file, as in the other projects.

## Conventions

- Next.js 16 and Fumadocs 16 differ from older versions. Read `node_modules/next/dist/docs/` and the types in `node_modules/fumadocs-*` before changing routes or layouts.
- The site is a static export. Route handlers export `dynamic = 'force-static'`. Plain URLs to files in `public/` go through `asset()` so they get the base path.
- Keep project values in `docs.config.ts`. Code reads them from there.
- Follow `docs/design.md`. The project colour never fills a panel, card, button or callout. Only the value of a badge is filled with it.
- Changes to the theme and components come from the docs base. Make them there first, then bring them here.

<!-- vale off -->
<!-- BEGIN:nextjs-agent-rules -->

## This is NOT the Next.js you know

This version has breaking changes — APIs, conventions, and file structure may all differ from your training data. Read the relevant guide in `node_modules/next/dist/docs/` (resolved from this file's directory; in monorepos the `next` package may not be visible from the repo root) before writing any code. Heed deprecation notices.

This block is written and re-added by `next dev` — verify at `node_modules/next/dist/server/lib/generate-agent-files.js`. Removing it from a diff only re-creates the uncommitted change; committing it with your work keeps the tree clean.

<!-- END:nextjs-agent-rules -->
<!-- vale on -->
