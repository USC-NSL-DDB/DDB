import {themes as prismThemes} from 'prism-react-renderer'

const repositoryUrl = 'https://github.com/USC-NSL-DDB/DDB'

export default {
  title: 'DDB API',
  tagline: 'Schemas, transports, and SDKs for DDB API v2',
  favicon: 'img/ddb-logo.png',
  url: 'https://usc-nsl-ddb.github.io',
  baseUrl: '/DDB/',
  organizationName: 'USC-NSL-DDB',
  projectName: 'DDB',
  trailingSlash: true,
  onBrokenLinks: 'throw',
  onBrokenAnchors: 'throw',
  markdown: {
    hooks: {
      onBrokenMarkdownLinks: 'throw',
    },
  },
  staticDirectories: ['static', '.generated/static'],

  presets: [
    [
      'classic',
      {
        blog: false,
        docs: {
          path: '.generated/sdk',
          routeBasePath: 'sdk',
          sidebarPath: './sidebarsSdk.mjs',
        },
        sitemap: {
          changefreq: 'weekly',
          priority: 0.5,
        },
        theme: {
          customCss: './src/css/custom.css',
        },
      },
    ],
  ],

  plugins: [
    [
      '@docusaurus/plugin-content-docs',
      {
        id: 'schema',
        path: '.generated/schema',
        routeBasePath: 'schema',
        sidebarPath: './sidebarsSchema.mjs',
        showLastUpdateAuthor: false,
        showLastUpdateTime: false,
      },
    ],
  ],

  themeConfig: {
    colorMode: {
      defaultMode: 'dark',
      respectPrefersColorScheme: true,
    },
    navbar: {
      title: 'DDB API',
      logo: {
        alt: 'DDB',
        src: 'img/ddb-logo.png',
      },
      items: [
        {to: '/schema/', label: 'Data schema', position: 'left'},
        {to: 'pathname:///openapi/', label: 'HTTP API', position: 'left'},
        {to: 'pathname:///asyncapi/', label: 'Event API', position: 'left'},
        {to: '/sdk/', label: 'SDKs', position: 'left'},
        {
          href: `${repositoryUrl}/tree/dev/ddb/docs/api`,
          label: 'API guides',
          position: 'right',
        },
        {
          href: repositoryUrl,
          label: 'GitHub',
          position: 'right',
        },
      ],
    },
    footer: {
      style: 'dark',
      links: [
        {
          title: 'References',
          items: [
            {label: 'Data schema', to: '/schema/'},
            {label: 'HTTP / OpenAPI', to: 'pathname:///openapi/'},
            {label: 'Events / AsyncAPI', to: 'pathname:///asyncapi/'},
          ],
        },
        {
          title: 'Client resources',
          items: [
            {label: 'SDKs', to: '/sdk/'},
            {label: 'Machine-readable artifacts', to: '/specs/'},
            {
              label: 'Contributing',
              href: `${repositoryUrl}/blob/dev/ddb/docs/api/contributing.md`,
            },
          ],
        },
        {
          title: 'Project',
          items: [
            {label: 'DDB repository', href: repositoryUrl},
            {
              label: 'General documentation',
              href: `${repositoryUrl}/tree/dev/ddb/docs`,
            },
          ],
        },
      ],
      copyright:
        'DDB API reference · generated from Protobuf and transport specifications',
    },
    prism: {
      theme: prismThemes.github,
      darkTheme: prismThemes.dracula,
    },
  },
}
