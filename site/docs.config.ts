/**
 * Everything that differs between the documentation sites of the organisation.
 *
 * The values below describe Parrot. The rest of the site follows
 * aiviarium/docs-base.
 */

export interface NavLink {
  text: string;
  url: string;
  /** Opens outside the site. Links that start with http are external anyway. */
  external?: boolean;
}

export interface Feature {
  title: string;
  description: string;
  url?: string;
}

/** One image in a dark and a light version, as paths under public/. */
export interface Themed {
  dark: string;
  light: string;
}

export interface DocsConfig {
  /** The name in title case, as it appears in prose: "Parrot". */
  name: string;
  /** The lowercase name used in URLs, packages and commands: "parrot". */
  slug: string;
  /** One sentence, shown under the wordmark and in the page metadata. */
  description: string;
  /** The area after "Aiviarium /" in the small label above the wordmark. */
  area: string;
  /** The project colour. `light` is the darker tone that passes 4.5:1 on white. */
  colour: Themed;
  /** The repository, linked from the header and the "Edit on GitHub" links. */
  repo: string;
  /** The folder of the docs inside the repository, for "Edit on GitHub". */
  repoDocsPath: string;
  /**
   * The badge beside the name in the header, such as the version. Drawn like the
   * organisation's badges: the label on a dark field, the value on the project
   * colour. Leave it out to hide it.
   */
  chip?: { label: string; value: string };
  /**
   * Badges from the organisation's badge endpoint, shown on the landing page and
   * in the README.
   */
  badges: {
    /** The badge endpoint. */
    host: string;
    /** The file name of a GitHub Actions workflow, for a CI badge. */
    ci?: string;
    /** Show the latest GitHub release. */
    release?: boolean;
  };
  install: {
    label: string;
    command: string;
  };
  /** The two buttons under the description. The first one is outlined in the project colour. */
  actions: [NavLink, NavLink];
  assets: {
    /** The detailed icon, shown at 128 px on the landing page. */
    icon: Themed;
    /** The `-icon` file, drawn for 24 to 48 px, used in the header lockup. */
    navIcon: Themed;
    /** The `-16` file, drawn on the pixel grid for 16 px: the favicon. */
    favicon: Themed;
    /** The faceted wordmark, for word sizes of 24 px and above. */
    wordmark: Themed;
    /** The plain wordmark in `currentColor` (`-mono`), for the 18 px header lockup. */
    wordmarkSolid: string;
  };
  /** Links in the header, after the name. */
  nav: NavLink[];
  /** Cards on the landing page. Leave the list empty to hide them. */
  features: Feature[];
}

const config: DocsConfig = {
  name: 'Parrot',
  slug: 'parrot',
  description: 'Agent CLIs in one runtime: drivers, terminals and protocol translators.',
  area: 'Coding agents',
  colour: { dark: '#FF6369', light: '#D1343B' },
  repo: 'https://github.com/aiviarium/parrot',
  repoDocsPath: 'docs',
  badges: {
    host: 'https://aiviarium-badges.helight.workers.dev',
    ci: 'ci.yml',
  },
  install: {
    label: 'Install with cargo',
    command: 'cargo install --locked --git https://github.com/aiviarium/parrot parrot-cli',
  },
  actions: [
    { text: 'Get started', url: '/docs/quick-start' },
    { text: 'Command line', url: '/docs/reference/cli' },
  ],
  assets: {
    icon: { dark: '/brand/parrot.svg', light: '/brand/parrot-light.svg' },
    navIcon: { dark: '/brand/parrot-icon.svg', light: '/brand/parrot-icon-light.svg' },
    favicon: { dark: '/brand/parrot-16.svg', light: '/brand/parrot-16-light.svg' },
    wordmark: {
      dark: '/brand/parrot-wordmark.svg',
      light: '/brand/parrot-wordmark-light.svg',
    },
    wordmarkSolid: '/brand/parrot-wordmark-mono.svg',
  },
  nav: [
    { text: 'Guide', url: '/docs' },
    { text: 'Drivers', url: '/docs/drivers/claude' },
    { text: 'Command line', url: '/docs/reference/cli' },
    { text: 'Protocol', url: '/docs/reference/protocol' },
  ],
  features: [
    {
      title: 'Every agent, one model',
      description:
        'Text, thinking, tool calls, approvals, questions and plans in the same items and states, whichever CLI produced them.',
      url: '/docs/items',
    },
    {
      title: 'Drivers from eggs',
      description:
        'Each agent is a Hatchery package with its driver data. Install one and its agent appears; remove it and the agent is gone.',
      url: '/docs/concepts#eggs',
    },
    {
      title: 'Terminals as a fallback',
      description:
        'A CLI with no protocol to speak is driven through its screen, with a profile that names states and actions.',
      url: '/docs/drivers/tui',
    },
    {
      title: 'Links between runtimes',
      description:
        'Two runtimes pair by key, see each other\'s environment and sessions, and meet over SSH, a command or TCP.',
      url: '/docs/links',
    },
    {
      title: 'For clients and driver authors',
      description:
        'Build a UI or an orchestrator on one protocol, or add a driver for another CLI with a package and its data files.',
      url: '/docs',
    },
    {
      title: 'Part of the Aiviarium stack',
      description:
        'Parrot drives the agents, Rookery provisions the machines, Crow is the product and Hatchery packages the drivers.',
      url: '/docs#how-it-fits-together',
    },
  ],
};

export default config;
