import Link from '@docusaurus/Link'
import Layout from '@theme/Layout'
import Heading from '@theme/Heading'

import styles from './index.module.css'

const references = [
  {
    title: 'Data schema',
    format: 'Protobuf',
    coverage:
      'Messages, fields, enums, oneofs, field presence, ProtoJSON names, type references, and HTTP operation usage.',
    useFor:
      'Implementing data models, serialization, validation, or generated bindings.',
    to: '/schema/',
  },
  {
    title: 'HTTP API',
    format: 'OpenAPI',
    coverage:
      'Unary endpoints, authorization scopes, headers, request and response bodies, status codes, and error responses.',
    useFor:
      'Implementing the HTTP/ProtoJSON transport or inspecting individual operations.',
    to: 'pathname:///openapi/',
  },
  {
    title: 'Event API',
    format: 'AsyncAPI',
    coverage:
      'State and output stream channels, NDJSON payloads, cursors, replay behavior, and stream lifecycle.',
    useFor:
      'Implementing live state synchronization, output subscriptions, and stream recovery.',
    to: 'pathname:///asyncapi/',
  },
  {
    title: 'SDKs',
    format: 'Rust, TypeScript, Python',
    coverage:
      'Client configuration, capability negotiation, unary calls, pagination, operation polling, and reconnecting streams.',
    useFor:
      'Using or extending a first-party client instead of implementing the transport directly.',
    to: '/sdk/',
  },
]

export default function Home() {
  return (
    <Layout
      title="API reference"
      description="Technical reference for the DDB API v2 data schema, HTTP operations, event streams, and client SDKs."
    >
      <main className={styles.page}>
        <article
          className={'container theme-doc-markdown markdown ' + styles.document}
        >
          <header className={styles.header}>
            <Heading as="h1">DDB API reference</Heading>
            <p className={styles.introduction}>
              This site documents the public DDB API v2 data model, HTTP
              operations, event streams, and first-party client libraries. It
              is an API reference; project overviews, user guides, and
              deployment documentation are maintained separately.
            </p>
          </header>

          <section aria-labelledby="reference-coverage">
            <Heading id="reference-coverage" as="h2">
              Reference coverage
            </Heading>
            <p>
              The API is described through several formats. Use the data schema
              for shared types, then select the transport or SDK reference that
              matches your implementation.
            </p>
            <div className={styles.tableWrapper}>
              <table className={styles.referenceTable}>
                <thead>
                  <tr>
                    <th>Reference</th>
                    <th>Coverage</th>
                    <th>Use it for</th>
                  </tr>
                </thead>
                <tbody>
                  {references.map((reference) => (
                    <tr key={reference.title}>
                      <td className={styles.referenceName}>
                        <Link to={reference.to}>{reference.title}</Link>
                        <span>{reference.format}</span>
                      </td>
                      <td>{reference.coverage}</td>
                      <td>{reference.useFor}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </section>

          <section aria-labelledby="reference-order">
            <Heading id="reference-order" as="h2">
              How to use these references
            </Heading>
            <ol>
              <li>
                Start with the <Link to="/schema/">data schema</Link> for
                message definitions, field semantics, and ProtoJSON encoding.
              </li>
              <li>
                Use <Link to="pathname:///openapi/">OpenAPI</Link> for unary
                HTTP operations and{' '}
                <Link to="pathname:///asyncapi/">AsyncAPI</Link> for event
                streams.
              </li>
              <li>
                Use the <Link to="/sdk/">SDK reference</Link> for
                language-specific setup and client behavior.
              </li>
            </ol>
          </section>

          <section aria-labelledby="machine-readable-files">
            <Heading id="machine-readable-files" as="h2">
              Machine-readable API artifacts
            </Heading>
            <p>
              The <Link to="/specs/">artifact index</Link> publishes the
              Protobuf sources, descriptor set, OpenAPI and AsyncAPI files,
              operation metadata, checksums, and source-revision metadata used
              to build this documentation.
            </p>
          </section>
        </article>
      </main>
    </Layout>
  )
}
