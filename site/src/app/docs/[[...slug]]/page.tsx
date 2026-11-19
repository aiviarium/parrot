import type { Metadata } from 'next';
import { notFound } from 'next/navigation';
import {
  DocsBody,
  DocsDescription,
  DocsPage,
  DocsTitle,
  EditOnGitHub,
  MarkdownCopyButton,
  PageLastUpdate,
  ViewOptionsPopover,
} from 'fumadocs-ui/layouts/notebook/page';
import { createRelativeLink } from 'fumadocs-ui/mdx';
import { getMDXComponents } from '@/components/mdx';
import { markdownUrl } from '@/lib/llms';
import { editUrl } from '@/lib/site';
import { source } from '@/lib/source';

export default async function Page(props: PageProps<'/docs/[[...slug]]'>) {
  const page = source.getPage((await props.params).slug);
  if (!page) notFound();
  const MDX = page.data.body;
  const { lastModified } = page.data;
  return (
    <DocsPage toc={page.data.toc} tableOfContent={{ style: 'clerk' }}>
      <DocsTitle>{page.data.title}</DocsTitle>
      <DocsDescription className="mb-0">{page.data.description}</DocsDescription>
      <div className="flex flex-row flex-wrap items-center gap-2 border-b pb-6">
        <MarkdownCopyButton markdownUrl={markdownUrl(page)} className="nc-btn" />
        <ViewOptionsPopover
          markdownUrl={markdownUrl(page)}
          githubUrl={editUrl(page.path)}
          className="nc-btn"
        />
      </div>
      <DocsBody>
        {/* Links between pages can be written as file names, such as ./deploying.mdx. */}
        <MDX components={getMDXComponents({ a: createRelativeLink(source, page) })} />
      </DocsBody>
      <div className="flex flex-row flex-wrap items-center justify-between gap-4 border-t pt-4">
        <EditOnGitHub href={editUrl(page.path)} />
        {lastModified && <PageLastUpdate date={new Date(lastModified)} />}
      </div>
    </DocsPage>
  );
}

export function generateStaticParams() {
  return source.generateParams();
}

export async function generateMetadata(props: PageProps<'/docs/[[...slug]]'>): Promise<Metadata> {
  const page = source.getPage((await props.params).slug);
  if (!page) notFound();
  return { title: page.data.title, description: page.data.description };
}
