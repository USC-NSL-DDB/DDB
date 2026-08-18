import Layout from '@theme/Layout'
import Heading from '@theme/Heading'
import useBaseUrl from '@docusaurus/useBaseUrl'

import styles from './index.module.css'

const contractArtifacts = [
  {
    path: 'openapi-v2.json',
    title: 'OpenAPI',
    detail: 'HTTP endpoints, authorization, status codes, and ProtoJSON schemas.',
  },
  {
    path: 'asyncapi-v2.json',
    title: 'AsyncAPI',
    detail: 'State and output stream channels, message schemas, and bindings.',
  },
  {
    path: 'operation-registry-v2.json',
    title: 'Operation registry',
    detail: 'Generated HTTP, permission, error, and streaming metadata.',
  },
  {
    path: 'operation-policy-v2.json',
    title: 'Operation policy',
    detail: 'Source policy for HTTP bindings, permissions, errors, and streaming.',
  },
]

const schemaArtifacts = [
  {
    path: 'ddb-api-v2-descriptor.binpb',
    title: 'Descriptor set',
    detail: 'Compiled FileDescriptorSet for reflection, code generation, and schema inspection.',
  },
  {
    path: 'schema-reference-v2.json',
    title: 'Schema reference JSON',
    detail: 'Generated index of messages, fields, enums, operations, and type references.',
  },
  {
    path: 'buf.yaml',
    title: 'Buf configuration',
    detail: 'Module, lint, and breaking-change settings for the Protobuf sources.',
  },
]

const protoFiles = [
  'common.proto',
  'extension.proto',
  'resources.proto',
  'debugger_service.proto',
  'event_service.proto',
]

function Artifact({artifact, base}) {
  return (
    <a className={styles.artifact} href={`${base}${artifact.path}`}>
      <strong>{artifact.title}</strong>
      <code>{artifact.path}</code>
      <span>{artifact.detail}</span>
    </a>
  )
}

export default function Specs() {
  const specsBase = useBaseUrl('/specs/')
  return (
    <Layout
      title="API artifacts"
      description="Versioned schemas, metadata, and Protobuf sources for DDB API v2."
    >
      <main className="container margin-vert--xl">
        <header className={styles.header}>
          <span>Published files</span>
          <Heading as="h1">Machine-readable API artifacts</Heading>
          <p>
            These files are the source inputs and generated reference
            outputs for DDB API v2. Verify downloads with the checksum manifest
            and record the source revision from the build metadata.
          </p>
          <div className={styles.manifests}>
            <a href={`${specsBase}checksums.txt`}>SHA-256 checksum manifest</a>
            <a href={`${specsBase}build-metadata.json`}>Build metadata</a>
          </div>
        </header>

        <section className={styles.section}>
          <Heading as="h2">Transport schemas and metadata</Heading>
          <div className={styles.grid}>
            {contractArtifacts.map((artifact) => (
              <Artifact key={artifact.path} artifact={artifact} base={specsBase} />
            ))}
          </div>
        </section>

        <section className={styles.section}>
          <Heading as="h2">Schema tooling</Heading>
          <div className={styles.grid}>
            {schemaArtifacts.map((artifact) => (
              <Artifact key={artifact.path} artifact={artifact} base={specsBase} />
            ))}
          </div>
        </section>

        <section className={styles.section}>
          <Heading as="h2">Protobuf source files</Heading>
          <p>
            Paths are preserved relative to the published Buf configuration so
            tools can compile the files without rewriting imports.
          </p>
          <ul className={styles.protoList}>
            {protoFiles.map((file) => {
              const path = `proto/ddb/api/v2/${file}`
              return (
                <li key={file}>
                  <a href={`${specsBase}${path}`}>
                    <code>{path}</code>
                  </a>
                </li>
              )
            })}
          </ul>
        </section>
      </main>
    </Layout>
  )
}
