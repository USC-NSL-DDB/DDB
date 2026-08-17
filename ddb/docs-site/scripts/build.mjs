import { createHash } from 'node:crypto'
import { copyFile, mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import path from 'node:path'

import {
  contractSummary,
  contracts,
  docsSiteRoot,
  runTool,
} from './common.mjs'

const dist = path.join(docsSiteRoot, 'dist')
const specs = path.join(dist, 'specs')
const staticRoot = path.join(docsSiteRoot, 'static')
const redocPackageRoot = path.join(
  docsSiteRoot,
  'redoc-runtime',
  'node_modules',
  'redoc',
)

if (path.dirname(dist) !== docsSiteRoot || path.basename(dist) !== 'dist') {
  throw new Error('refusing to clean unexpected output directory ' + dist)
}

async function readJson(file) {
  return JSON.parse(await readFile(file, 'utf8'))
}

function sourceRevision() {
  const revision = process.env.GITHUB_SHA
  return typeof revision === 'string' && /^[0-9a-f]{40}$/i.test(revision)
    ? revision.toLowerCase()
    : null
}

const [openapi, asyncapi, registry] = await Promise.all([
  readJson(contracts.openapi),
  readJson(contracts.asyncapi),
  readJson(contracts.registry),
])
const summary = contractSummary(openapi, asyncapi, registry)
const revision = sourceRevision()

await rm(dist, { recursive: true, force: true })
await Promise.all([
  mkdir(path.join(dist, 'assets'), { recursive: true }),
  mkdir(path.join(dist, 'openapi'), { recursive: true }),
  mkdir(path.join(dist, 'asyncapi'), { recursive: true }),
  mkdir(specs, { recursive: true }),
])
await Promise.all([
  copyFile(path.join(staticRoot, 'index.html'), path.join(dist, 'index.html')),
  writeFile(path.join(dist, '.nojekyll'), ''),
])

runTool('redocly', [
  'build-docs',
  contracts.openapi,
  '--output',
  path.join(dist, 'openapi', 'index.html'),
  '--disableGoogleFont',
])

const redocOutput = path.join(dist, 'openapi', 'index.html')
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
await Promise.all([
  writeFile(
    redocOutput,
    redocHtml.replace(
      redocCdnPattern,
      '<script src="../assets/redoc.standalone.js"></script>',
    ),
  ),
  copyFile(
    path.join(redocPackageRoot, 'bundles', 'redoc.standalone.js'),
    path.join(dist, 'assets', 'redoc.standalone.js'),
  ),
])

runTool('asyncapi', [
  'generate',
  'fromTemplate',
  contracts.asyncapi,
  '@asyncapi/html-template',
  '--output',
  path.join(dist, 'asyncapi'),
  '--force-write',
  '--no-interactive',
  '--param',
  'singleFile=true',
])

const publishedContracts = [
  ['openapi-v2.json', contracts.openapi],
  ['asyncapi-v2.json', contracts.asyncapi],
  ['operation-registry-v2.json', contracts.registry],
]
await Promise.all(
  publishedContracts.map(([name, source]) =>
    copyFile(source, path.join(specs, name)),
  ),
)

const checksums = []
for (const [name] of publishedContracts) {
  const bytes = await readFile(path.join(specs, name))
  checksums.push(
    createHash('sha256').update(bytes).digest('hex') + '  ' + name,
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
      inputs: {
        openapi: 'ddb/docs/api/generated/openapi-v2.json',
        asyncapi: 'ddb/docs/api/generated/asyncapi-v2.json',
        operationRegistry:
          'ddb/docs/api/generated/operation-registry-v2.json',
      },
    },
    null,
    2,
  ) + '\n',
)

console.log(
  [
    'Built DDB API ',
    summary.apiVersion,
    ' OpenAPI and AsyncAPI references at ',
    dist,
  ].join(''),
)
