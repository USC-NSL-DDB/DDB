import {createHash} from 'node:crypto'
import {copyFile, mkdir, readFile, rm, writeFile} from 'node:fs/promises'
import path from 'node:path'

import {
  contractSummary,
  contracts,
  ddbRoot,
  docsSiteRoot,
  runTool,
} from './common.mjs'
import {
  generatePortalContent,
  portalPaths,
  protoSources,
} from './generate-portal-content.mjs'

const dist = path.join(docsSiteRoot, 'dist')
const generatedStatic = portalPaths.staticRoot
const specs = path.join(generatedStatic, 'specs')
const redocPackageRoot = path.join(
  docsSiteRoot,
  'redoc-runtime',
  'node_modules',
  'redoc',
)

const standaloneHead = `
  <link rel="icon" type="image/png" href="../img/ddb-logo.png">
  <style>
    .ddb-api-header {
      align-items: center;
      background: #fff;
      border-bottom: 1px solid #dbe4ee;
      box-sizing: border-box;
      display: flex;
      font-family: Inter, ui-sans-serif, system-ui, -apple-system,
        BlinkMacSystemFont, "Segoe UI", sans-serif;
      gap: 2rem;
      height: 4.25rem;
      padding: 0 1.5rem;
    }
    .ddb-api-header a {
      align-items: center;
      color: #1c1e21;
      display: inline-flex;
      text-decoration: none;
    }
    .ddb-api-header__brand {
      flex: 0 0 auto;
      font-weight: 600;
      gap: 0.75rem;
    }
    .ddb-api-header__brand img {
      height: 2.15rem;
      width: 2.15rem;
    }
    .ddb-api-header nav {
      display: flex;
      gap: 1.5rem;
    }
    .ddb-api-header nav a {
      font-size: 0.95rem;
      font-weight: 500;
    }
    @media (max-width: 42rem) {
      .ddb-api-header nav {
        display: none;
      }
    }
    @media print {
      .ddb-api-header {
        display: none;
      }
    }
  </style>`

const standaloneHeader = `
  <header class="ddb-api-header">
    <a class="ddb-api-header__brand" href="../">
      <img src="../img/ddb-logo.png" alt="DDB">
      <span>DDB API</span>
    </a>
    <nav aria-label="API reference sections">
      <a href="../schema/">Data schema</a>
      <a href="../openapi/">HTTP API</a>
      <a href="../asyncapi/">Event API</a>
      <a href="../sdk/">SDKs</a>
    </nav>
  </header>`

const args = process.argv.slice(2)
const prepareOnly = args.length === 1 && args[0] === '--prepare-only'
if (args.length > 0 && !prepareOnly) {
  throw new Error(
    `usage: node scripts/build.mjs [--prepare-only], received: ${args.join(' ')}`,
  )
}

if (path.dirname(dist) !== docsSiteRoot || path.basename(dist) !== 'dist') {
  throw new Error('refusing to clean unexpected output directory ' + dist)
}

async function readJson(file) {
  return JSON.parse(await readFile(file, 'utf8'))
}

function brandStandaloneReference(html, name) {
  const headClose = html.indexOf('</head>')
  if (headClose < 0) {
    throw new Error(name + ' output has no closing head element')
  }

  const faviconPattern =
    /<link\b(?=[^>]*\brel=["'](?:shortcut )?icon["'])[^>]*\/?>\s*/gi
  const head = html.slice(0, headClose).replace(faviconPattern, '')
  let branded = head + standaloneHead + '\n' + html.slice(headClose)

  const bodyOpen = branded.match(/<body(?:\s[^>]*)?>/i)
  if (!bodyOpen || bodyOpen.index === undefined) {
    throw new Error(name + ' output has no opening body element')
  }
  const insertionPoint = bodyOpen.index + bodyOpen[0].length
  branded =
    branded.slice(0, insertionPoint) +
    standaloneHeader +
    branded.slice(insertionPoint)

  return branded
}

function sourceRevision() {
  const revision = process.env.GITHUB_SHA
  return typeof revision === 'string' && /^[0-9a-f]{40}$/i.test(revision)
    ? revision.toLowerCase()
    : null
}

async function publishArtifact(artifact) {
  const destination = path.join(specs, artifact.name)
  await mkdir(path.dirname(destination), {recursive: true})
  if (artifact.source) {
    await copyFile(artifact.source, destination)
  } else {
    await writeFile(destination, artifact.bytes)
  }
}

const [openapi, asyncapi, registry] = await Promise.all([
  readJson(contracts.openapi),
  readJson(contracts.asyncapi),
  readJson(contracts.registry),
])
const summary = contractSummary(openapi, asyncapi, registry)
const revision = sourceRevision()

if (!prepareOnly) {
  await rm(dist, {recursive: true, force: true})
}
const portal = await generatePortalContent({apiVersion: summary.apiVersion})
await Promise.all([
  mkdir(path.join(generatedStatic, 'assets'), {recursive: true}),
  mkdir(path.join(generatedStatic, 'openapi'), {recursive: true}),
  mkdir(path.join(generatedStatic, 'asyncapi'), {recursive: true}),
  mkdir(specs, {recursive: true}),
])

runTool('redocly', [
  'build-docs',
  contracts.openapi,
  '--output',
  path.join(generatedStatic, 'openapi', 'index.html'),
  '--disableGoogleFont',
])

const redocOutput = path.join(generatedStatic, 'openapi', 'index.html')
const redocHtml = await readFile(redocOutput, 'utf8')
const redocCdnPattern =
  /<script src="https:\/\/cdn\.redocly\.com\/redoc\/v([^/"?]+)\/bundles\/redoc\.standalone\.js"[^>]*><\/script>/g
const redocCdnMatches = [...redocHtml.matchAll(redocCdnPattern)]
if (redocCdnMatches.length !== 1) {
  throw new Error(
    'expected exactly one Redoc CDN script, found ' + redocCdnMatches.length,
  )
}
const redocPackage = JSON.parse(
  await readFile(path.join(redocPackageRoot, 'package.json'), 'utf8'),
)
if (redocCdnMatches[0][1] !== redocPackage.version) {
  throw new Error(
    'Redocly renderer and local Redoc versions disagree: ' +
      redocCdnMatches[0][1] +
      ' / ' +
      redocPackage.version,
  )
}
const bundledRedocHtml = redocHtml.replace(
  redocCdnPattern,
  '<script src="../assets/redoc.standalone.js"></script>',
)
await Promise.all([
  writeFile(
    redocOutput,
    brandStandaloneReference(bundledRedocHtml, 'Redoc'),
  ),
  copyFile(
    path.join(redocPackageRoot, 'bundles/redoc.standalone.js'),
    path.join(generatedStatic, 'assets/redoc.standalone.js'),
  ),
])

runTool('asyncapi', [
  'generate',
  'fromTemplate',
  contracts.asyncapi,
  '@asyncapi/html-template',
  '--output',
  path.join(generatedStatic, 'asyncapi'),
  '--force-write',
  '--no-interactive',
  '--param',
  'singleFile=true',
])

const asyncapiOutput = path.join(generatedStatic, 'asyncapi', 'index.html')
await writeFile(
  asyncapiOutput,
  brandStandaloneReference(
    await readFile(asyncapiOutput, 'utf8'),
    'AsyncAPI',
  ),
)

const artifacts = [
  {
    name: 'openapi-v2.json',
    source: contracts.openapi,
  },
  {
    name: 'asyncapi-v2.json',
    source: contracts.asyncapi,
  },
  {
    name: 'operation-registry-v2.json',
    source: contracts.registry,
  },
  {
    name: 'operation-policy-v2.json',
    source: path.join(
      ddbRoot,
      'proto/ddb/api/v2/operation_policy.json',
    ),
  },
  {
    name: 'ddb-api-v2-descriptor.binpb',
    bytes: portal.descriptorBytes,
  },
  {
    name: 'schema-reference-v2.json',
    bytes: portal.catalogBytes,
  },
  {
    name: 'buf.yaml',
    source: path.join(ddbRoot, 'buf.yaml'),
  },
  ...protoSources.map((source) => ({
    name: path.posix.join('proto', source),
    source: path.join(ddbRoot, 'proto', source),
  })),
]
await Promise.all(artifacts.map(publishArtifact))

const checksums = []
for (const artifact of [...artifacts].sort((left, right) =>
  left.name.localeCompare(right.name),
)) {
  const bytes = await readFile(path.join(specs, artifact.name))
  checksums.push(
    createHash('sha256').update(bytes).digest('hex') + '  ' + artifact.name,
  )
}
await writeFile(path.join(specs, 'checksums.txt'), checksums.join('\n') + '\n')
await writeFile(
  path.join(specs, 'build-metadata.json'),
  JSON.stringify(
    {
      apiVersion: summary.apiVersion,
      sourceRevision: revision,
      sourceRepository: 'https://github.com/USC-NSL-DDB/DDB',
      operationCount: summary.operationCount,
      streamChannelCount: summary.streamChannelCount,
      schema: {
        package: portal.catalog.package,
        messageCount: portal.catalog.counts.messages,
        enumCount: portal.catalog.counts.enums,
      },
      sdks: ['rust', 'typescript', 'python'],
      inputs: {
        openapi: 'ddb/docs/api/generated/openapi-v2.json',
        asyncapi: 'ddb/docs/api/generated/asyncapi-v2.json',
        operationRegistry:
          'ddb/docs/api/generated/operation-registry-v2.json',
        operationPolicy: 'ddb/proto/ddb/api/v2/operation_policy.json',
        descriptor:
          'ddb/api-types/descriptor/ddb_api_v2_descriptor.bin',
        protobufSources: protoSources.map((source) => `ddb/proto/${source}`),
      },
    },
    null,
    2,
  ) + '\n',
)

if (!prepareOnly) {
  runTool('docusaurus', ['build', '--out-dir', dist])
  await writeFile(path.join(dist, '.nojekyll'), '')
}

const action = prepareOnly ? 'Prepared' : 'Built'
const destination = prepareOnly ? generatedStatic : dist
console.log(
  [
    action,
    ' DDB API ',
    summary.apiVersion,
    ' portal with ',
    portal.catalog.counts.messages,
    ' messages, ',
    portal.catalog.counts.enums,
    ' enums, and ',
    summary.operationCount,
    ' operations at ',
    destination,
  ].join(''),
)
