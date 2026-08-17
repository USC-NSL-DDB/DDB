import assert from 'node:assert/strict'
import { readFile, stat } from 'node:fs/promises'
import path from 'node:path'
import test from 'node:test'

import { contracts, docsSiteRoot } from '../scripts/common.mjs'

const dist = path.join(docsSiteRoot, 'dist')
const specs = path.join(dist, 'specs')

test('build emits only reference pages and contract artifacts', async () => {
  const required = [
    '.nojekyll',
    'index.html',
    'assets/redoc.standalone.js',
    'openapi/index.html',
    'asyncapi/index.html',
    'specs/openapi-v2.json',
    'specs/asyncapi-v2.json',
    'specs/operation-registry-v2.json',
    'specs/checksums.txt',
    'specs/build-metadata.json',
  ]
  for (const file of required) {
    const metadata = await stat(path.join(dist, file))
    assert.ok(metadata.isFile(), file + ' must be a regular file')
  }
})

test('root is a relative redirect rather than a second landing page', async () => {
  const root = await readFile(path.join(dist, 'index.html'), 'utf8')
  assert.match(root, /http-equiv="refresh" content="0; url=\.\/openapi\/"/)
  assert.match(root, /href="\.\/openapi\/"/)
  assert.doesNotMatch(root, /(?:href|src)="\/openapi\//)
})

test('published contracts are exact byte copies', async () => {
  const pairs = [
    [contracts.openapi, path.join(specs, 'openapi-v2.json')],
    [contracts.asyncapi, path.join(specs, 'asyncapi-v2.json')],
    [contracts.registry, path.join(specs, 'operation-registry-v2.json')],
  ]
  for (const [source, published] of pairs) {
    assert.deepEqual(await readFile(published), await readFile(source))
  }
})

test('standalone references contain no third-party executable imports', async () => {
  const openapiPage = await readFile(
    path.join(dist, 'openapi/index.html'),
    'utf8',
  )
  const pages = [
    openapiPage,
    await readFile(path.join(dist, 'asyncapi/index.html'), 'utf8'),
  ]
  assert.match(openapiPage, /src="\.\.\/assets\/redoc\.standalone\.js"/)
  for (const page of pages) {
    assert.ok(page.length > 50_000, 'reference page should contain bundled UI')
    assert.doesNotMatch(page, /<script[^>]+src=["'](?:https?:)?\/\//i)
  }
})

test('published Redoc runtime is the pinned local bundle', async () => {
  assert.deepEqual(
    await readFile(path.join(dist, 'assets/redoc.standalone.js')),
    await readFile(
      path.join(
        docsSiteRoot,
        'redoc-runtime',
        'node_modules/redoc/bundles/redoc.standalone.js',
      ),
    ),
  )
})

test('build metadata and checksums describe the published contracts', async () => {
  const metadata = JSON.parse(
    await readFile(path.join(specs, 'build-metadata.json'), 'utf8'),
  )
  const openapi = JSON.parse(
    await readFile(path.join(specs, 'openapi-v2.json'), 'utf8'),
  )
  const asyncapi = JSON.parse(
    await readFile(path.join(specs, 'asyncapi-v2.json'), 'utf8'),
  )
  const checksums = await readFile(path.join(specs, 'checksums.txt'), 'utf8')

  assert.equal(metadata.apiVersion, openapi.info.version)
  assert.equal(metadata.operationCount, Object.keys(openapi.paths).length)
  assert.equal(
    metadata.streamChannelCount,
    Object.keys(asyncapi.channels).length,
  )
  assert.ok(
    metadata.sourceRevision === null ||
      /^[0-9a-f]{40}$/.test(metadata.sourceRevision),
  )
  for (const file of [
    'openapi-v2.json',
    'asyncapi-v2.json',
    'operation-registry-v2.json',
  ]) {
    assert.match(
      checksums,
      new RegExp('^[0-9a-f]{64}  ' + file + '$', 'm'),
    )
  }
})
