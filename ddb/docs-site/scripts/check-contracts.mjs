import { readFile } from 'node:fs/promises'

import { contractSummary, contracts, runTool } from './common.mjs'

async function readJson(file) {
  return JSON.parse(await readFile(file, 'utf8'))
}

const [openapi, asyncapi, registry] = await Promise.all([
  readJson(contracts.openapi),
  readJson(contracts.asyncapi),
  readJson(contracts.registry),
])

if (openapi.openapi !== '3.1.0') {
  throw new Error('expected OpenAPI 3.1.0, found ' + openapi.openapi)
}
if (asyncapi.asyncapi !== '3.1.0') {
  throw new Error('expected AsyncAPI 3.1.0, found ' + asyncapi.asyncapi)
}
if (registry.schemaVersion !== 1) {
  throw new Error(
    'expected operation registry schema 1, found ' + registry.schemaVersion,
  )
}

const summary = contractSummary(openapi, asyncapi, registry)
runTool('redocly', ['lint', contracts.openapi])
runTool('asyncapi', [
  'validate',
  contracts.asyncapi,
  '--diagnostics-format',
  'json',
])

console.log(
  [
    'DDB API ',
    summary.apiVersion,
    ': ',
    summary.operationCount,
    ' HTTP operations and ',
    summary.streamChannelCount,
    ' stream channels validated',
  ].join(''),
)
