import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import path from 'node:path'

export const docsSiteRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '..',
)
export const ddbRoot = path.resolve(docsSiteRoot, '..')
export const contracts = Object.freeze({
  openapi: path.join(ddbRoot, 'docs/api/generated/openapi-v2.json'),
  asyncapi: path.join(ddbRoot, 'docs/api/generated/asyncapi-v2.json'),
  registry: path.join(
    ddbRoot,
    'docs/api/generated/operation-registry-v2.json',
  ),
})

function executable(name) {
  const suffix = process.platform === 'win32' ? '.cmd' : ''
  return path.join(docsSiteRoot, 'node_modules', '.bin', name + suffix)
}

export function runTool(name, args) {
  const asyncApiEnvironment =
    name === 'asyncapi'
      ? {
          CI: 'true',
          NODE_CONFIG_ENV: 'development',
        }
      : {}
  const result = spawnSync(executable(name), args, {
    cwd: docsSiteRoot,
    env: {
      ...process.env,
      ...asyncApiEnvironment,
      REDOCLY_SUPPRESS_UPDATE_NOTICE: 'true',
      SUPPRESS_NO_CONFIG_WARNING: 'true',
    },
    shell: false,
    stdio: 'inherit',
  })
  if (result.error) {
    throw new Error('failed to start ' + name + ': ' + result.error.message)
  }
  if (result.status !== 0) {
    throw new Error(name + ' exited with status ' + result.status)
  }
}

export function contractSummary(openapi, asyncapi, registry) {
  const openapiVersion = openapi?.info?.version
  const asyncapiVersion = asyncapi?.info?.version
  if (
    typeof openapiVersion !== 'string' ||
    openapiVersion.length === 0 ||
    openapiVersion !== asyncapiVersion
  ) {
    throw new Error(
      'OpenAPI and AsyncAPI versions disagree: ' +
        openapiVersion +
        ' / ' +
        asyncapiVersion,
    )
  }
  const operationCount = Object.keys(openapi?.paths ?? {}).length
  const streamChannelCount = Object.keys(asyncapi?.channels ?? {}).length
  const registryOperationCount = Array.isArray(registry?.operations)
    ? registry.operations.length
    : -1
  if (operationCount === 0 || operationCount !== registryOperationCount) {
    throw new Error(
      'OpenAPI and registry operation counts disagree: ' +
        operationCount +
        ' / ' +
        registryOperationCount,
    )
  }
  if (streamChannelCount === 0) {
    throw new Error('AsyncAPI must contain at least one stream channel')
  }
  return { apiVersion: openapiVersion, operationCount, streamChannelCount }
}
