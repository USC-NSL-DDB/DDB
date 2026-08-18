import {mkdir, readFile, rm, writeFile} from 'node:fs/promises'
import path from 'node:path'

import protobuf from 'protobufjs'
import descriptor from 'protobufjs/ext/descriptor/index.js'

import {contracts, ddbRoot, docsSiteRoot} from './common.mjs'

const PACKAGE = 'ddb.api.v2'
const PACKAGE_PREFIX = `.${PACKAGE}.`
const repository = 'https://github.com/USC-NSL-DDB/DDB'
const descriptorPath = path.join(
  ddbRoot,
  'api-types/descriptor/ddb_api_v2_descriptor.bin',
)
const generatedRoot = path.join(docsSiteRoot, '.generated')
const schemaDocsRoot = path.join(generatedRoot, 'schema')
const sdkDocsRoot = path.join(generatedRoot, 'sdk')

export const protoSources = Object.freeze([
  'ddb/api/v2/common.proto',
  'ddb/api/v2/extension.proto',
  'ddb/api/v2/resources.proto',
  'ddb/api/v2/debugger_service.proto',
  'ddb/api/v2/event_service.proto',
])

function normalizeTypeName(name) {
  return String(name).replace(/^\./, '')
}

function shortName(fullName) {
  return fullName.slice(fullName.lastIndexOf('.') + 1)
}

function slugFor(name) {
  return name
    .replace(/([a-z0-9])([A-Z])/g, '$1-$2')
    .replace(/[._]/g, '-')
    .toLowerCase()
}

function cleanComment(comment) {
  if (typeof comment !== 'string') {
    return ''
  }
  return comment
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean)
    .join(' ')
}

function markdownText(value) {
  return String(value)
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('{', '&#123;')
    .replaceAll('}', '&#125;')
}

function markdownCell(value) {
  return markdownText(value).replaceAll('|', '\\|').replaceAll('\n', '<br />')
}

function frontMatter({title, description, slug, position}) {
  const lines = ['---', `title: ${JSON.stringify(title)}`]
  if (description) {
    lines.push(`description: ${JSON.stringify(description)}`)
  }
  if (slug) {
    lines.push(`slug: ${slug}`)
  }
  if (position !== undefined) {
    lines.push(`sidebar_position: ${position}`)
  }
  lines.push('---', '')
  return lines.join('\n')
}

async function readJson(file) {
  return JSON.parse(await readFile(file, 'utf8'))
}

function buildSourceRoot(sourceDocuments) {
  const root = new protobuf.Root()
  for (const {file, source} of sourceDocuments) {
    protobuf.parse(source, root, {
      alternateCommentMode: true,
      filename: file,
      keepCase: true,
      preferTrailingComment: false,
    })
  }
  return root
}

function indexDescriptorSources(descriptorSet) {
  const sources = new Map()

  function indexEnums(enums, prefix, file) {
    for (const item of enums ?? []) {
      sources.set(`${prefix}.${item.name}`, file)
    }
  }

  function indexMessages(messages, prefix, file) {
    for (const item of messages ?? []) {
      const fullName = `${prefix}.${item.name}`
      sources.set(fullName, file)
      indexEnums(item.enumType, fullName, file)
      indexMessages(item.nestedType, fullName, file)
    }
  }

  for (const file of descriptorSet.file) {
    if (file.package !== PACKAGE) {
      continue
    }
    const prefix = `.${file.package}`
    indexEnums(file.enumType, prefix, file.name)
    indexMessages(file.messageType, prefix, file.name)
  }

  return sources
}

function collectPublicTypes(namespace) {
  const messages = []
  const enums = []

  function visit(item) {
    if (item instanceof protobuf.Type) {
      if (item.options?.map_entry !== true && item.options?.mapEntry !== true) {
        messages.push(item)
      }
      for (const nested of item.nestedArray ?? []) {
        visit(nested)
      }
      return
    }
    if (item instanceof protobuf.Enum) {
      enums.push(item)
      return
    }
    for (const nested of item.nestedArray ?? []) {
      visit(nested)
    }
  }

  for (const item of namespace.nestedArray ?? []) {
    visit(item)
  }

  const inPackage = (item) => item.fullName.startsWith(PACKAGE_PREFIX)
  return {
    messages: messages.filter(inPackage),
    enums: enums.filter(inPackage),
  }
}

const jsonScalar = Object.freeze({
  double: 'number',
  float: 'number',
  int32: 'number',
  sint32: 'number',
  sfixed32: 'number',
  uint32: 'number',
  fixed32: 'number',
  int64: 'decimal string',
  sint64: 'decimal string',
  sfixed64: 'decimal string',
  uint64: 'decimal string',
  fixed64: 'decimal string',
  bool: 'boolean',
  string: 'string',
  bytes: 'base64 string',
})

function jsonRepresentation(field) {
  const resolved = field.resolvedType?.fullName
  let base
  if (resolved === '.google.protobuf.Timestamp') {
    base = 'RFC 3339 string'
  } else if (resolved === '.google.protobuf.Duration') {
    base = 'duration string'
  } else if (field.resolvedType instanceof protobuf.Enum) {
    base = 'enum name string'
  } else if (field.resolvedType instanceof protobuf.Type) {
    base = 'object'
  } else {
    base = jsonScalar[field.type] ?? field.type
  }

  if (field.map) {
    return `object<string, ${base}>`
  }
  if (field.repeated) {
    return `array<${base}>`
  }
  return base
}

function fieldPresence(field) {
  if (field.map) {
    return 'map'
  }
  if (field.repeated) {
    return 'repeated'
  }
  if (field.partOf && field.options?.proto3_optional !== true) {
    return `oneof ${field.partOf.name}`
  }
  if (field.options?.proto3_optional === true) {
    return 'explicit optional'
  }
  if (field.resolvedType instanceof protobuf.Type) {
    return 'message presence'
  }
  return 'implicit scalar'
}

function descriptorFieldType(field) {
  const resolved = field.resolvedType?.fullName
  const valueType = resolved ? normalizeTypeName(resolved) : field.type
  return {
    display: field.map
      ? `map<${field.keyType}, ${shortName(valueType)}>`
      : shortName(valueType),
    typeFullName:
      resolved?.startsWith(PACKAGE_PREFIX) === true
        ? normalizeTypeName(resolved)
        : null,
  }
}

function operationSummary(operation) {
  return {
    key: operation.key,
    operationId: operation.operationId,
    description: operation.description,
    httpMethod: operation.httpMethod.toUpperCase(),
    path: operation.path,
    permission: operation.permission,
    requestType: normalizeTypeName(operation.requestType),
    responseType: normalizeTypeName(operation.responseType),
    serverStreaming: operation.serverStreaming,
    successStatus: operation.successStatus,
  }
}

function buildSchemaCatalog({
  apiVersion,
  descriptorRoot,
  sourceRoot,
  descriptorSources,
  registry,
}) {
  descriptorRoot.resolveAll()
  const namespace = descriptorRoot.lookup(PACKAGE)
  const publicTypes = collectPublicTypes(namespace)
  const operations = registry.operations.map(operationSummary)
  const operationsByRequest = new Map()
  const operationsByResponse = new Map()

  for (const operation of operations) {
    const request = operationsByRequest.get(operation.requestType) ?? []
    request.push(operation)
    operationsByRequest.set(operation.requestType, request)
    const response = operationsByResponse.get(operation.responseType) ?? []
    response.push(operation)
    operationsByResponse.set(operation.responseType, response)
  }

  const messages = publicTypes.messages
    .map((type) => {
      const fullName = normalizeTypeName(type.fullName)
      const sourceType = sourceRoot.lookup(fullName)
      if (!(sourceType instanceof protobuf.Type)) {
        throw new Error(`Protobuf source is missing message ${fullName}`)
      }
      const description = cleanComment(sourceType.comment)
      if (!description) {
        throw new Error(`public message ${fullName} needs a source comment`)
      }
      const source = descriptorSources.get(type.fullName)
      if (!source) {
        throw new Error(`descriptor source is missing message ${fullName}`)
      }

      const fields = type.fieldsArray
        .map((field) => {
          field.resolve()
          const sourceField = sourceType.fields[field.protoName]
          if (!sourceField) {
            throw new Error(
              `Protobuf source is missing field ${fullName}.${field.protoName}`,
            )
          }
          const sourceOneof =
            field.partOf && field.options?.proto3_optional !== true
              ? sourceType.oneofs[field.partOf.name]
              : null
          const fieldDescription =
            cleanComment(sourceField.comment) ||
            cleanComment(sourceOneof?.comment)
          if (!fieldDescription) {
            throw new Error(
              `public field ${fullName}.${field.protoName} needs a source comment`,
            )
          }
          const fieldType = descriptorFieldType(field)
          return {
            number: field.id,
            protoName: field.protoName,
            jsonName: field.jsonName,
            type: fieldType.display,
            typeFullName: fieldType.typeFullName,
            presence: fieldPresence(field),
            protojson: jsonRepresentation(field),
            oneof:
              field.partOf && field.options?.proto3_optional !== true
                ? field.partOf.name
                : null,
            deprecated: field.options?.deprecated === true,
            description: fieldDescription,
          }
        })
        .sort((left, right) => left.number - right.number)

      const oneofs = Object.values(type.oneofs ?? {})
        .filter(
          (oneof) =>
            !oneof.fieldsArray.every(
              (field) => field.options?.proto3_optional === true,
            ),
        )
        .map((oneof) => ({
          name: oneof.name,
          fields: oneof.fieldsArray.map((field) => field.protoName),
          description: cleanComment(sourceType.oneofs[oneof.name]?.comment),
        }))

      for (const oneof of oneofs) {
        if (!oneof.description) {
          throw new Error(
            `public oneof ${fullName}.${oneof.name} needs a source comment`,
          )
        }
      }

      return {
        kind: 'message',
        name: type.name,
        fullName,
        slug: slugFor(fullName.slice(PACKAGE.length + 1)),
        source,
        description,
        deprecated: type.options?.deprecated === true,
        fields,
        oneofs,
        requestOperations: operationsByRequest.get(fullName) ?? [],
        responseOperations: operationsByResponse.get(fullName) ?? [],
        referencedBy: [],
      }
    })
    .sort((left, right) => left.fullName.localeCompare(right.fullName))

  const enums = publicTypes.enums
    .map((type) => {
      const fullName = normalizeTypeName(type.fullName)
      const sourceType = sourceRoot.lookup(fullName)
      if (!(sourceType instanceof protobuf.Enum)) {
        throw new Error(`Protobuf source is missing enum ${fullName}`)
      }
      const description = cleanComment(sourceType.comment)
      if (!description) {
        throw new Error(`public enum ${fullName} needs a source comment`)
      }
      const source = descriptorSources.get(type.fullName)
      if (!source) {
        throw new Error(`descriptor source is missing enum ${fullName}`)
      }
      return {
        kind: 'enum',
        name: type.name,
        fullName,
        slug: slugFor(fullName.slice(PACKAGE.length + 1)),
        source,
        description,
        deprecated: type.options?.deprecated === true,
        values: Object.entries(type.values).map(([name, number]) => ({
          name,
          number,
          description: cleanComment(sourceType.comments?.[name]),
        })),
        referencedBy: [],
      }
    })
    .sort((left, right) => left.fullName.localeCompare(right.fullName))

  const symbols = new Map(
    [...messages, ...enums].map((item) => [item.fullName, item]),
  )
  for (const message of messages) {
    for (const field of message.fields) {
      if (!field.typeFullName) {
        continue
      }
      const target = symbols.get(field.typeFullName)
      if (!target) {
        throw new Error(
          `${message.fullName}.${field.protoName} references undocumented ${field.typeFullName}`,
        )
      }
      target.referencedBy.push({
        message: message.fullName,
        field: field.protoName,
      })
    }
  }

  for (const operation of operations) {
    for (const typeName of [operation.requestType, operation.responseType]) {
      if (!symbols.has(typeName)) {
        throw new Error(
          `operation ${operation.key} references undocumented ${typeName}`,
        )
      }
    }
  }

  return {
    schemaVersion: apiVersion,
    package: PACKAGE,
    generatedFrom: {
      descriptor: 'api-types/descriptor/ddb_api_v2_descriptor.bin',
      protobufSources: protoSources,
      operationRegistry: path.relative(ddbRoot, contracts.registry),
    },
    counts: {
      messages: messages.length,
      enums: enums.length,
      operations: operations.length,
    },
    messages,
    enums,
    operations,
  }
}

function sourceLink(source) {
  return `${repository}/blob/dev/ddb/proto/${source}`
}

function typeLink(typeFullName, display, currentKind, symbols) {
  if (!typeFullName) {
    return `\`${display}\``
  }
  const target = symbols.get(typeFullName)
  if (!target) {
    return `\`${display}\``
  }
  const directory = target.kind === 'message' ? 'messages' : 'enums'
  const href =
    currentKind === target.kind
      ? `./${target.slug}.mdx`
      : `../${directory}/${target.slug}.mdx`
  return `[${markdownText(display)}](${href})`
}

function operationLink(operation) {
  const service = operation.key.split('.')[0]
  const href = `pathname:///openapi/#tag/${service}/operation/${operation.operationId}`
  return `[${operation.key}](${href})`
}

function renderOperationUsage(message) {
  const sections = []
  if (message.requestOperations.length > 0) {
    sections.push(
      '### Used as a request',
      '',
      ...message.requestOperations.map(
        (operation) =>
          `- ${operationLink(operation)} — \`${operation.httpMethod} ${operation.path}\``,
      ),
      '',
    )
  }
  if (message.responseOperations.length > 0) {
    sections.push(
      '### Used as a response',
      '',
      ...message.responseOperations.map(
        (operation) =>
          `- ${operationLink(operation)} — HTTP ${operation.successStatus}`,
      ),
      '',
    )
  }
  return sections
}

function renderMessagePage(message, symbols) {
  const lines = [
    frontMatter({
      title: message.name,
      description: message.description,
    }),
    `# ${message.name}`,
    '',
    `\`message ${message.fullName}\``,
    '',
    message.deprecated ? '> **Deprecated.** Do not use in new integrations.\n' : '',
    markdownText(message.description),
    '',
    `**Protobuf source:** [\`${message.source}\`](${sourceLink(message.source)})`,
    '',
    '## Fields',
    '',
  ]

  if (message.fields.length === 0) {
    lines.push('This message has no fields.', '')
  } else {
    lines.push(
      '| # | Protobuf field | JSON field | Type | Presence | ProtoJSON | Description |',
      '|---:|---|---|---|---|---|---|',
    )
    for (const field of message.fields) {
      const type = typeLink(
        field.typeFullName,
        field.type,
        'message',
        symbols,
      )
      const deprecated = field.deprecated ? '**Deprecated.** ' : ''
      lines.push(
        `| ${field.number} | \`${field.protoName}\` | \`${field.jsonName}\` | ${type} | ${markdownCell(field.presence)} | ${markdownCell(field.protojson)} | ${deprecated}${markdownCell(field.description)} |`,
      )
    }
    lines.push('')
  }

  if (message.oneofs.length > 0) {
    lines.push('## Oneofs', '')
    for (const oneof of message.oneofs) {
      const description = oneof.description
        ? ` — ${markdownText(oneof.description)}`
        : ''
      lines.push(
        `- \`${oneof.name}\`: exactly one of ${oneof.fields
          .map((field) => `\`${field}\``)
          .join(', ')}${description}`,
      )
    }
    lines.push('')
  }

  const usage = renderOperationUsage(message)
  if (usage.length > 0) {
    lines.push('## HTTP operation usage', '', ...usage)
  }

  if (message.referencedBy.length > 0) {
    lines.push('## Referenced by', '')
    for (const reference of message.referencedBy) {
      const parent = symbols.get(reference.message)
      lines.push(
        `- [${shortName(reference.message)}](./${parent.slug}.mdx).\`${reference.field}\``,
      )
    }
    lines.push('')
  }

  return lines.filter((line) => line !== null).join('\n')
}

function renderEnumPage(item, symbols) {
  const lines = [
    frontMatter({
      title: item.name,
      description: item.description,
    }),
    `# ${item.name}`,
    '',
    `\`enum ${item.fullName}\``,
    '',
    item.deprecated ? '> **Deprecated.** Do not use in new integrations.\n' : '',
    markdownText(item.description),
    '',
    '> ProtoJSON emits enum symbols as strings. Clients must tolerate symbols added by newer compatible servers.',
    '',
    `**Protobuf source:** [\`${item.source}\`](${sourceLink(item.source)})`,
    '',
    '## Values',
    '',
    '| Number | JSON / Protobuf symbol | Description |',
    '|---:|---|---|',
  ]
  for (const value of item.values) {
    lines.push(
      `| ${value.number} | \`${value.name}\` | ${markdownCell(value.description || '—')} |`,
    )
  }
  lines.push('')

  if (item.referencedBy.length > 0) {
    lines.push('## Referenced by', '')
    for (const reference of item.referencedBy) {
      const parent = symbols.get(reference.message)
      lines.push(
        `- [${shortName(reference.message)}](../messages/${parent.slug}.mdx).\`${reference.field}\``,
      )
    }
    lines.push('')
  }

  return lines.join('\n')
}

function renderOperationsPage(catalog, symbols) {
  const lines = [
    frontMatter({
      title: 'Operation type map',
      description:
        'DDB operations linked to their request and response message definitions.',
      slug: '/operations',
      position: 3,
    }),
    '# Operation type map',
    '',
    'This table maps each public operation to its HTTP endpoint, permission, request type, response type, and delivery mode. See OpenAPI for headers, status codes, and error responses.',
    '',
    '| Operation | HTTP | Scope | Request | Response | Delivery |',
    '|---|---|---|---|---|---|',
  ]

  for (const operation of catalog.operations) {
    const request = symbols.get(operation.requestType)
    const response = symbols.get(operation.responseType)
    const requestLink = `[\`${shortName(operation.requestType)}\`](./messages/${request.slug}.mdx)`
    const responseLink = `[\`${shortName(operation.responseType)}\`](./messages/${response.slug}.mdx)`
    lines.push(
      `| ${operationLink(operation)} | \`${operation.httpMethod}\` | \`${operation.permission}\` | ${requestLink} | ${responseLink} | ${operation.serverStreaming ? 'NDJSON stream' : `HTTP ${operation.successStatus}`} |`,
    )
  }
  lines.push('')
  return lines.join('\n')
}

function renderSchemaIndex(catalog) {
  const sourceLines = protoSources.map(
    (file) =>
      `- [\`${file}\`](pathname:///specs/proto/${file})`,
  )
  return [
    frontMatter({
      title: 'DDB data schema',
      description:
        'Protobuf messages and enums used by DDB API v2.',
      slug: '/',
      position: 1,
    }),
    '# DDB data schema',
    '',
    `DDB API **${catalog.schemaVersion}** defines **${catalog.counts.messages} messages**, **${catalog.counts.enums} enums**, and **${catalog.counts.operations} public operations** in \`${catalog.package}\`.`,
    '',
    'The checked-in Protobuf descriptor defines structure and field identifiers. `.proto` comments define field semantics. The operation registry defines HTTP, authorization, and streaming usage. The build fails if these inputs disagree.',
    '',
    '## Browse',
    '',
    '- [Messages](./category/messages/)',
    '- [Enums](./category/enums/)',
    '- [Operation type map](./operations.mdx)',
    '- [ProtoJSON mapping](./protojson.mdx)',
    '- [HTTP / OpenAPI reference](pathname:///openapi/)',
    '- [Event / AsyncAPI reference](pathname:///asyncapi/)',
    '',
    '## Protobuf source files',
    '',
    ...sourceLines,
    '',
    'Download the [descriptor set](pathname:///specs/ddb-api-v2-descriptor.binpb), [schema reference JSON](pathname:///specs/schema-reference-v2.json), or [artifact index](/specs/).',
    '',
  ].join('\n')
}

function renderProtoJsonPage() {
  return [
    frontMatter({
      title: 'ProtoJSON mapping',
      description:
        'ProtoJSON encoding rules for DDB HTTP bodies and NDJSON stream records.',
      slug: '/protojson',
      position: 2,
    }),
    '# ProtoJSON mapping',
    '',
    'DDB HTTP request and response bodies and NDJSON stream records use the ProtoJSON field names shown on each message page.',
    '',
    '## Encoding rules',
    '',
    '- Field names are lower camel case in JSON; parsers should also accept the original Protobuf field name.',
    '- Signed and unsigned 64-bit integers are decimal JSON strings.',
    '- Enum values are emitted as symbolic strings. New symbols may appear in compatible server releases.',
    '- `bytes` values are base64 strings.',
    '- `google.protobuf.Timestamp` values are RFC 3339 strings.',
    '- `google.protobuf.Duration` values use the Protobuf duration string form.',
    '- Except for `google.protobuf.NullValue`, parsers treat `null` as an unset field; serializers omit unset fields.',
    '- Unknown JSON fields are ignored by the first-party decoders for forward compatibility, subject to DDB payload limits.',
    '',
    'See the [Protobuf JSON mapping](https://protobuf.dev/programming-guides/json/) for the language-neutral format and the [DDB compatibility policy](https://github.com/USC-NSL-DDB/DDB/blob/dev/ddb/docs/api/compatibility.md) for DDB’s evolution rules.',
    '',
  ].join('\n')
}

function removeReadmeHeading(readme) {
  return readme.replace(/^# .+\n+/, '')
}

async function generateSdkDocs() {
  const sdkSources = [
    {
      id: 'rust',
      title: 'Rust SDK',
      description: 'Asynchronous Rust client for DDB API v2.',
      source: path.join(ddbRoot, 'api-client/README.md'),
      sourceUrl: `${repository}/blob/dev/ddb/api-client/README.md`,
      position: 2,
      transform(content) {
        return content.replace(
          '](../docs/api/',
          `](${repository}/blob/dev/ddb/docs/api/`,
        )
      },
    },
    {
      id: 'typescript',
      title: 'TypeScript SDK',
      description:
        'HTTP/ProtoJSON client with no runtime dependencies for Node.js and browsers.',
      source: path.join(ddbRoot, 'sdk/typescript/README.md'),
      sourceUrl: `${repository}/blob/dev/ddb/sdk/typescript/README.md`,
      position: 3,
    },
    {
      id: 'python',
      title: 'Python SDK',
      description: 'Python 3.11+ standard-library client for HTTP/ProtoJSON.',
      source: path.join(ddbRoot, 'sdk/python/README.md'),
      sourceUrl: `${repository}/blob/dev/ddb/sdk/python/README.md`,
      position: 4,
    },
  ]

  const index = [
    frontMatter({
      title: 'DDB SDKs',
      description:
        'First-party Rust, TypeScript, and Python clients for DDB API v2.',
      slug: '/',
      position: 1,
    }),
    '# DDB SDKs',
    '',
    'All first-party SDKs use the generated Protobuf model and HTTP/ProtoJSON transport. They implement authentication, deadlines, typed errors, bounded pagination, operation polling, and reconnecting event streams.',
    '',
    '| SDK | Runtime requirements | Common uses |',
    '|---|---|---|',
    '| [Rust](./rust.md) | Tokio and Reqwest | Rust applications and native frontends |',
    '| [TypeScript](./typescript.md) | Node.js 18+ or browser Fetch APIs | Web applications, extensions, and Node.js tooling |',
    '| [Python](./python.md) | Python 3.11+, standard library runtime | Automation, notebooks, and IDE tooling |',
    '',
    'Package availability depends on the DDB release. Each page links to the package source and documents current compatibility behavior.',
    '',
    'Clients must complete the server and capability handshake before enabling optional features. HTTP/ProtoJSON is the required baseline transport.',
    '',
  ]
  await writeFile(path.join(sdkDocsRoot, 'index.md'), index.join('\n'))

  for (const sdk of sdkSources) {
    let body = removeReadmeHeading(await readFile(sdk.source, 'utf8'))
    if (sdk.transform) {
      body = sdk.transform(body)
    }
    const page = [
      frontMatter({
        title: sdk.title,
        description: sdk.description,
        slug: `/${sdk.id}`,
        position: sdk.position,
      }),
      `# ${sdk.title}`,
      '',
      `[Package source](${sdk.sourceUrl})`,
      '',
      body.trim(),
      '',
    ].join('\n')
    await writeFile(path.join(sdkDocsRoot, `${sdk.id}.md`), page)
  }
}

export async function generatePortalContent({apiVersion}) {
  const [descriptorBytes, registry, ...sourceContents] = await Promise.all([
    readFile(descriptorPath),
    readJson(contracts.registry),
    ...protoSources.map((file) =>
      readFile(path.join(ddbRoot, 'proto', file), 'utf8'),
    ),
  ])
  const descriptorSet = descriptor.FileDescriptorSet.decode(descriptorBytes)
  const descriptorRoot = protobuf.Root.fromDescriptor(descriptorSet)
  const sourceDocuments = protoSources.map((file, index) => ({
    file,
    source: sourceContents[index],
  }))
  const sourceRoot = buildSourceRoot(sourceDocuments)
  const descriptorSources = indexDescriptorSources(descriptorSet)

  const catalog = buildSchemaCatalog({
    apiVersion,
    descriptorRoot,
    sourceRoot,
    descriptorSources,
    registry,
  })

  await rm(generatedRoot, {recursive: true, force: true})
  await Promise.all([
    mkdir(path.join(schemaDocsRoot, 'messages'), {recursive: true}),
    mkdir(path.join(schemaDocsRoot, 'enums'), {recursive: true}),
    mkdir(sdkDocsRoot, {recursive: true}),
    mkdir(path.join(generatedRoot, 'static'), {recursive: true}),
  ])

  const symbols = new Map(
    [...catalog.messages, ...catalog.enums].map((item) => [
      item.fullName,
      item,
    ]),
  )
  await Promise.all([
    writeFile(path.join(schemaDocsRoot, 'index.mdx'), renderSchemaIndex(catalog)),
    writeFile(
      path.join(schemaDocsRoot, 'protojson.mdx'),
      renderProtoJsonPage(),
    ),
    writeFile(
      path.join(schemaDocsRoot, 'operations.mdx'),
      renderOperationsPage(catalog, symbols),
    ),
    writeFile(
      path.join(schemaDocsRoot, 'messages/_category_.json'),
      JSON.stringify(
        {label: 'Messages', position: 4, link: {type: 'generated-index'}},
        null,
        2,
      ) + '\n',
    ),
    writeFile(
      path.join(schemaDocsRoot, 'enums/_category_.json'),
      JSON.stringify(
        {label: 'Enums', position: 5, link: {type: 'generated-index'}},
        null,
        2,
      ) + '\n',
    ),
    generateSdkDocs(),
  ])

  await Promise.all([
    ...catalog.messages.map((message) =>
      writeFile(
        path.join(schemaDocsRoot, 'messages', `${message.slug}.mdx`),
        renderMessagePage(message, symbols),
      ),
    ),
    ...catalog.enums.map((item) =>
      writeFile(
        path.join(schemaDocsRoot, 'enums', `${item.slug}.mdx`),
        renderEnumPage(item, symbols),
      ),
    ),
  ])

  return {
    catalog,
    catalogBytes: Buffer.from(JSON.stringify(catalog, null, 2) + '\n', 'utf8'),
    descriptorBytes,
  }
}

export const portalPaths = Object.freeze({
  descriptor: descriptorPath,
  generatedRoot,
  staticRoot: path.join(generatedRoot, 'static'),
})
