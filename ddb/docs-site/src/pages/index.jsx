import clsx from 'clsx'
import Link from '@docusaurus/Link'
import Layout from '@theme/Layout'
import Heading from '@theme/Heading'

import styles from './index.module.css'

const references = [
  {
    eyebrow: 'Canonical model',
    title: 'Data schema',
    description:
      'Browse DDB messages, fields, enums, oneofs, ProtoJSON names, and every HTTP operation that consumes each type.',
    to: '/schema/',
    action: 'Explore the schema',
  },
  {
    eyebrow: 'Request / response',
    title: 'HTTP API',
    description:
      'Use the OpenAPI reference for routes, authorization, status codes, request bodies, and response envelopes.',
    to: 'pathname:///openapi/',
    action: 'Open the HTTP reference',
  },
  {
    eyebrow: 'Streaming',
    title: 'Event API',
    description:
      'Use the AsyncAPI reference for replayable state changes, output records, cursors, and stream lifecycle.',
    to: 'pathname:///asyncapi/',
    action: 'Open the event reference',
  },
]

function ReferenceCard({reference}) {
  return (
    <article className={styles.card}>
      <span className={styles.eyebrow}>{reference.eyebrow}</span>
      <Heading as="h2">{reference.title}</Heading>
      <p>{reference.description}</p>
      <Link className={styles.cardLink} to={reference.to}>
        {reference.action} <span aria-hidden="true">→</span>
      </Link>
    </article>
  )
}

export default function Home() {
  return (
    <Layout
      title="API reference"
      description="Canonical schemas, transport contracts, and SDK references for the DDB debugger backend."
    >
      <main>
        <header className={styles.hero}>
          <div className="container">
            <div className={styles.heroCopy}>
              <span className={styles.kicker}>DDB developer platform</span>
              <Heading as="h1">Build a debugger frontend on a stable contract.</Heading>
              <p>
                Protobuf defines DDB&apos;s semantic data model. OpenAPI defines
                HTTP/ProtoJSON delivery, and AsyncAPI defines event streams.
                Start with the contract your integration needs.
              </p>
              <div className={styles.actions}>
                <Link className="button button--primary button--lg" to="/schema/">
                  Browse data schema
                </Link>
                <Link className="button button--secondary button--lg" to="/sdk/">
                  Choose an SDK
                </Link>
              </div>
            </div>
          </div>
        </header>

        <section className={clsx('container', styles.references)}>
          <div className={styles.sectionHeading}>
            <span className={styles.kicker}>One API, distinct views</span>
            <Heading as="h2">Authoritative references without competing contracts</Heading>
          </div>
          <div className={styles.grid}>
            {references.map((reference) => (
              <ReferenceCard key={reference.title} reference={reference} />
            ))}
          </div>
        </section>

        <section className={styles.contractStrip}>
          <div className={clsx('container', styles.contractGrid)}>
            <div>
              <span className={styles.kicker}>For automation</span>
              <Heading as="h2">Consume the exact generated artifacts.</Heading>
            </div>
            <p>
              Download the versioned Protobuf sources, descriptor set,
              OpenAPI, AsyncAPI, and operation registry with checksums and
              build provenance.
            </p>
            <Link className="button button--outline button--lg" to="/specs/">
              View artifacts
            </Link>
          </div>
        </section>
      </main>
    </Layout>
  )
}
