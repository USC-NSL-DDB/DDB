# DDB API reference site

This directory builds two public, read-only reference pages from DDB's
checked-in generated contracts:

- /openapi/: standalone Redoc CE reference for HTTP/ProtoJSON operations;
- /asyncapi/: standalone AsyncAPI reference for state and output streams;
- /specs/: raw OpenAPI, AsyncAPI, operation-registry, checksum, and provenance
  files.

The root page only redirects to /openapi/. DDB's existing project landing page
should link directly to /openapi/ and /asyncapi/.

This site is a projection, not another source of API truth. It never rewrites
files under docs/api/generated, and rendered HTML under dist is not committed.

## Local verification

Node.js 24 LTS with npm 11.17 is required. CI and .node-version pin the exact
current LTS release, 24.19.0, so tool behavior does not change between runs.

    npm ci --ignore-scripts
    npm ci --ignore-scripts --prefix redoc-runtime
    npm run check
    npm run build
    npm test
    python3 -m http.server --directory dist 8080

Open http://127.0.0.1:8080/openapi/ or
http://127.0.0.1:8080/asyncapi/.

npm run check runs lockfile-pinned Redocly and AsyncAPI validators. The
AsyncAPI CLI's optional Studio dependency is overridden to 1.3.0 because its
newer 1.4.0 release currently references an unpublished package. npm test
confirms that both self-hosted references exist, raw contracts are
byte-identical to their sources, provenance and checksums are correct, and
neither reference
loads executable code from a third-party origin. The build replaces Redocly's
CDN runtime tag with the matching pinned local Redoc bundle and rejects version
drift between them. Redoc has an isolated lockfile so its React dependencies
cannot affect the AsyncAPI generator.

## Deployment behavior

.github/workflows/api-docs-pages.yml validates builds on relevant pull requests
and pushes. It uploads and deploys a Pages artifact only from the repository's
current default branch. Feature branches cannot replace the public site. All
URLs are relative, so the artifact works under the GitHub project path (/DDB/)
and under a future custom domain.
