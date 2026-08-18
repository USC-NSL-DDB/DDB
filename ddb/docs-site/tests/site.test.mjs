import assert from 'node:assert/strict'
import {createHash} from 'node:crypto'
import {readdir, readFile, stat} from 'node:fs/promises'
import path from 'node:path'
import test from 'node:test'

import {contracts, ddbRoot, docsSiteRoot} from '../scripts/common.mjs'
import {protoSources} from '../scripts/generate-portal-content.mjs'

const dist = path.join(docsSiteRoot, 'dist')
const specs = path.join(dist, 'specs')
const siteOrigin = 'https://usc-nsl-ddb.github.io'
const baseUrl = '/DDB/'

const publishedSources = [
  {name: 'openapi-v2.json', source: contracts.openapi},
  {name: 'asyncapi-v2.json', source: contracts.asyncapi},
  {name: 'operation-registry-v2.json', source: contracts.registry},
  {
    name: 'operation-policy-v2.json',
    source: path.join(ddbRoot, 'proto/ddb/api/v2/operation_policy.json'),
  },
  {
    name: 'ddb-api-v2-descriptor.binpb',
    source: path.join(
      ddbRoot,
      'api-types/descriptor/ddb_api_v2_descriptor.bin',
    ),
  },
  {name: 'buf.yaml', source: path.join(ddbRoot, 'buf.yaml')},
  ...protoSources.map((source) => ({
    name: path.posix.join('proto', source),
    source: path.join(ddbRoot, 'proto', source),
  })),
]

async function readJson(file) {
  return JSON.parse(await readFile(file, 'utf8'))
}

async function assertRegularFile(file) {
  const metadata = await stat(file)
  assert.ok(metadata.isFile(), file + ' must be a regular file')
}

async function walkFiles(root, relative = '') {
  const directory = path.join(root, relative)
  const entries = await readdir(directory, {withFileTypes: true})
  const files = []
  for (const entry of entries) {
    const child = path.join(relative, entry.name)
    if (entry.isDirectory()) {
      files.push(...(await walkFiles(root, child)))
    } else if (entry.isFile()) {
      files.push(child.split(path.sep).join('/'))
    }
  }
  return files
}

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex')
}

function openApiOperationCount(openapi) {
  const methods = new Set([
    'get',
    'put',
    'post',
    'delete',
    'options',
    'head',
    'patch',
    'trace',
  ])
  return Object.values(openapi.paths).reduce(
    (count, pathItem) =>
      count +
      Object.keys(pathItem).filter((key) => methods.has(key)).length,
    0,
  )
}

function assertUnique(items, property, label) {
  const values = items.map((item) => item[property])
  assert.equal(new Set(values).size, values.length, label + ' must be unique')
}

test('build emits the complete API reference portal', async () => {
  const required = [
    '.nojekyll',
    '404.html',
    'index.html',
    'sitemap.xml',
    'assets/redoc.standalone.js',
    'openapi/index.html',
    'asyncapi/index.html',
    'schema/index.html',
    'schema/operations/index.html',
    'schema/protojson/index.html',
    'schema/category/messages/index.html',
    'schema/category/enums/index.html',
    'sdk/index.html',
    'sdk/rust/index.html',
    'sdk/typescript/index.html',
    'sdk/python/index.html',
    'specs/index.html',
    'specs/schema-reference-v2.json',
    'specs/build-metadata.json',
    'specs/checksums.txt',
    ...publishedSources.map((artifact) => `specs/${artifact.name}`),
  ]
  for (const file of new Set(required)) {
    await assertRegularFile(path.join(dist, file))
  }
})

test('root is a technical API documentation index', async () => {
  const root = await readFile(path.join(dist, 'index.html'), 'utf8')
  for (const snippet of [
    'DDB API reference',
    'Reference coverage',
    'How to use these references',
    'Machine-readable API artifacts',
  ]) {
    assert.ok(root.includes(snippet), `root must include ${snippet}`)
  }
  assert.doesNotMatch(root, /http-equiv=["']refresh/i)
  for (const route of ['schema', 'openapi', 'asyncapi', 'sdk', 'specs']) {
    assert.match(root, new RegExp(`href=["']${baseUrl}${route}/["']`))
  }
})

test('published source artifacts are exact byte copies', async () => {
  for (const artifact of publishedSources) {
    assert.deepEqual(
      await readFile(path.join(specs, artifact.name)),
      await readFile(artifact.source),
      artifact.name,
    )
  }
})

test('schema catalog is complete, cross-linked, and fully rendered', async () => {
  const catalog = await readJson(path.join(specs, 'schema-reference-v2.json'))
  const registry = await readJson(contracts.registry)
  const openapi = await readJson(contracts.openapi)
  const symbols = new Map(
    [...catalog.messages, ...catalog.enums].map((item) => [
      item.fullName,
      item,
    ]),
  )

  assert.equal(catalog.schemaVersion, openapi.info.version)
  assert.equal(catalog.package, 'ddb.api.v2')
  assert.equal(catalog.counts.messages, catalog.messages.length)
  assert.equal(catalog.counts.enums, catalog.enums.length)
  assert.equal(catalog.counts.operations, catalog.operations.length)
  assert.equal(catalog.operations.length, registry.operations.length)
  assert.deepEqual(catalog.generatedFrom.protobufSources, protoSources)
  assertUnique([...catalog.messages, ...catalog.enums], 'fullName', 'type names')
  assertUnique(catalog.messages, 'slug', 'message slugs')
  assertUnique(catalog.enums, 'slug', 'enum slugs')
  assertUnique(catalog.operations, 'key', 'operation keys')
  assertUnique(catalog.operations, 'operationId', 'operation IDs')

  for (const message of catalog.messages) {
    assert.equal(message.kind, 'message')
    assert.ok(message.description, message.fullName + ' needs a description')
    assert.ok(
      protoSources.includes(message.source),
      message.fullName + ' needs a Protobuf source',
    )
    assertUnique(message.fields, 'number', message.fullName + ' field numbers')
    assertUnique(
      message.fields,
      'protoName',
      message.fullName + ' Protobuf field names',
    )
    for (const field of message.fields) {
      assert.ok(
        field.description,
        `${message.fullName}.${field.protoName} needs a description`,
      )
      if (field.typeFullName) {
        assert.ok(
          symbols.has(field.typeFullName),
          `${message.fullName}.${field.protoName} references ${field.typeFullName}`,
        )
      }
    }
    for (const oneof of message.oneofs) {
      assert.ok(
        oneof.description,
        `${message.fullName}.${oneof.name} needs a description`,
      )
      const members = new Set(
        message.fields
          .filter((field) => field.oneof === oneof.name)
          .map((field) => field.protoName),
      )
      assert.deepEqual(new Set(oneof.fields), members)
    }
    for (const reference of message.referencedBy) {
      assert.ok(symbols.has(reference.message), reference.message)
    }
    const page = path.join(
      dist,
      'schema/messages',
      message.slug,
      'index.html',
    )
    await assertRegularFile(page)
    assert.match(await readFile(page, 'utf8'), new RegExp(message.fullName))
  }

  for (const item of catalog.enums) {
    assert.equal(item.kind, 'enum')
    assert.ok(item.description, item.fullName + ' needs a description')
    assert.ok(
      protoSources.includes(item.source),
      item.fullName + ' needs a Protobuf source',
    )
    assert.ok(item.values.length > 0, item.fullName + ' needs values')
    assertUnique(item.values, 'name', item.fullName + ' value names')
    assertUnique(item.values, 'number', item.fullName + ' value numbers')
    for (const reference of item.referencedBy) {
      assert.ok(symbols.has(reference.message), reference.message)
    }
    const page = path.join(dist, 'schema/enums', item.slug, 'index.html')
    await assertRegularFile(page)
    assert.match(await readFile(page, 'utf8'), new RegExp(item.fullName))
  }

  const registryByKey = new Map(
    registry.operations.map((operation) => [operation.key, operation]),
  )
  for (const operation of catalog.operations) {
    const source = registryByKey.get(operation.key)
    assert.ok(source, operation.key)
    assert.ok(symbols.has(operation.requestType), operation.requestType)
    assert.ok(symbols.has(operation.responseType), operation.responseType)
    assert.equal(operation.operationId, source.operationId)
    assert.equal(operation.path, source.path)
    assert.equal(operation.httpMethod, source.httpMethod.toUpperCase())
    assert.equal(operation.permission, source.permission)
    assert.equal(operation.serverStreaming, source.serverStreaming)
    assert.equal(operation.successStatus, source.successStatus)
  }
})

test('SDK pages are sourced from all first-party client packages', async () => {
  const pages = [
    {
      file: 'rust/index.html',
      snippets: [
        'Typed Rust client for DDB API v2 frontends.',
        'ClientConfig',
        'HTTP/ProtoJSON is the required baseline transport.',
      ],
    },
    {
      file: 'typescript/index.html',
      snippets: [
        'Dependency-free TypeScript client',
        'ExecutionActionValues',
        'isApiVersionUnavailable',
      ],
    },
    {
      file: 'python/index.html',
      snippets: [
        'Typed Python 3.11+ client',
        'ddb_api.generated.types',
        'is_api_version_unavailable',
      ],
    },
  ]
  for (const page of pages) {
    const html = await readFile(path.join(dist, 'sdk', page.file), 'utf8')
    for (const snippet of page.snippets) {
      assert.ok(html.includes(snippet), `${page.file} must include ${snippet}`)
    }
    assert.match(html, /Package source/)
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

test('metadata and checksums cover every published API artifact', async () => {
  const metadata = await readJson(path.join(specs, 'build-metadata.json'))
  const openapi = await readJson(path.join(specs, 'openapi-v2.json'))
  const asyncapi = await readJson(path.join(specs, 'asyncapi-v2.json'))
  const registry = await readJson(path.join(specs, 'operation-registry-v2.json'))
  const catalogBytes = await readFile(
    path.join(specs, 'schema-reference-v2.json'),
  )
  const catalog = JSON.parse(catalogBytes.toString('utf8'))

  assert.equal(metadata.apiVersion, openapi.info.version)
  assert.equal(metadata.operationCount, openApiOperationCount(openapi))
  assert.equal(metadata.operationCount, registry.operations.length)
  assert.equal(metadata.streamChannelCount, Object.keys(asyncapi.channels).length)
  assert.equal(metadata.schema.package, catalog.package)
  assert.equal(metadata.schema.messageCount, catalog.messages.length)
  assert.equal(metadata.schema.enumCount, catalog.enums.length)
  assert.deepEqual(metadata.sdks, ['rust', 'typescript', 'python'])
  assert.deepEqual(
    metadata.inputs.protobufSources,
    protoSources.map((source) => `ddb/proto/${source}`),
  )
  assert.ok(
    metadata.sourceRevision === null ||
      /^[0-9a-f]{40}$/.test(metadata.sourceRevision),
  )
  assert.deepEqual(
    catalogBytes,
    Buffer.from(JSON.stringify(catalog, null, 2) + '\n', 'utf8'),
  )

  const lines = (
    await readFile(path.join(specs, 'checksums.txt'), 'utf8')
  )
    .trim()
    .split('\n')
  const checksums = new Map()
  for (const line of lines) {
    const match = /^([0-9a-f]{64})  (.+)$/.exec(line)
    assert.ok(match, 'invalid checksum line: ' + line)
    assert.ok(!checksums.has(match[2]), 'duplicate checksum: ' + match[2])
    checksums.set(match[2], match[1])
  }

  const expected = [
    ...publishedSources.map((artifact) => artifact.name),
    'schema-reference-v2.json',
  ].sort()
  assert.deepEqual([...checksums.keys()], expected)
  for (const [name, digest] of checksums) {
    assert.equal(sha256(await readFile(path.join(specs, name))), digest, name)
  }
})

test('every rendered portal link stays under the base path and resolves', async () => {
  const htmlFiles = (await walkFiles(dist)).filter(
    (file) =>
      file.endsWith('.html') &&
      file !== 'openapi/index.html' &&
      file !== 'asyncapi/index.html',
  )
  const targetCache = new Map()

  async function targetExists(pathname) {
    if (targetCache.has(pathname)) {
      return targetCache.get(pathname)
    }
    const relative = decodeURIComponent(pathname.slice(baseUrl.length))
    const candidate = pathname.endsWith('/')
      ? path.join(dist, relative, 'index.html')
      : path.join(dist, relative)
    let result = false
    try {
      result = (await stat(candidate)).isFile()
    } catch (error) {
      if (!['ENOENT', 'ENOTDIR'].includes(error?.code)) {
        throw error
      }
    }
    targetCache.set(pathname, result)
    return result
  }

  for (const file of htmlFiles) {
    const html = await readFile(path.join(dist, file), 'utf8')
    const checkedMarkup = html.replace(
      /<link\b(?=[^>]*\brel=["'](?:canonical|alternate)["'])[^>]*>/gi,
      '',
    )
    const pagePath =
      file === 'index.html'
        ? baseUrl
        : baseUrl + file.replace(/index\.html$/, '')
    for (const match of checkedMarkup.matchAll(/\b(?:href|src)=["']([^"'<>]+)["']/g)) {
      const raw = match[1].replaceAll('&amp;', '&')
      if (!raw || raw.startsWith('#')) {
        continue
      }
      const target = new URL(raw, siteOrigin + pagePath)
      if (!['http:', 'https:'].includes(target.protocol)) {
        continue
      }
      if (target.origin !== siteOrigin) {
        continue
      }
      assert.ok(
        target.pathname.startsWith(baseUrl),
        `${file} escapes ${baseUrl}: ${raw}`,
      )
      assert.ok(
        await targetExists(target.pathname),
        `${file} links to missing ${target.pathname}`,
      )
    }
  }
})
