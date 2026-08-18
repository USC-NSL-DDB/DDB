import Layout from '@theme/Layout'
import Heading from '@theme/Heading'
import useBaseUrl from '@docusaurus/useBaseUrl'

import styles from './index.module.css'

const contractArtifacts = [
  {
    path: 'openapi-v2.json',
    title: 'OpenAPI',
    detail: 'HTTP routes, status codes, authentication, and ProtoJSON bodies.',
  },
  {
    path: 'asyncapi-v2.json',
    title: 'AsyncAPI',
    detail: 'State and output stream channels, messages, and bindings.',
  },
  {
    path: 'operation-registry-v2.json',
    title: 'Operation registry',
    detail: 'Resolved transport, permission, error, and streaming metadata.',
  },
  {
    path: 'operation-policy-v2.json',
    title: 'Operation policy',
    detail: 'Canonical authored transport policy consumed by generation.',
  },
]

const schemaArtifacts = [
  {
    path: 'ddb-api-v2-descriptor.binpb',
    title: 'Descriptor set',
    detail: 'Compiled FileDescriptorSet for reflection and client tooling.',
  },
  {
    path: 'schema-reference-v2.json',
    title: 'Schema catalog',
    detail: 'DDB-enriched messages, fields, enums, operations, and cross-links.',
  },
  {
    path: 'buf.yaml',
    title: 'Buf workspace',
    detail: 'Module, lint, and breaking-change policy for the published sources.',
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
      title="Contract artifacts"
      description="Versioned machine-readable contracts and Protobuf sources for the DDB API."
    >
      <main className="container margin-vert--xl">
        <header className={styles.header}>
          <span>Machine-readable distribution</span>
          <Heading as="h1">Contract artifacts</Heading>
          <p>
            These files are byte-for-byte build inputs or deterministic
            projections of the canonical DDB API contract. Verify downloads
            against the checksum manifest and record the source revision from
            build metadata.
          </p>
          <div className={styles.manifests}>
            <a href={`${specsBase}checksums.txt`}>SHA-256 checksums</a>
            <a href={`${specsBase}build-metadata.json`}>Build provenance</a>
          </div>
        </header>

        <section className={styles.section}>
          <Heading as="h2">Transport contracts</Heading>
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
          <Heading as="h2">Canonical Protobuf sources</Heading>
          <p>
            The directory layout is preserved so the files can be consumed
            with the published Buf workspace configuration.
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
