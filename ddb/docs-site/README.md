# DDB API reference portal

This directory builds the public API-reference site for developers creating DDB
frontends, SDKs, extensions, and integrations. It is deliberately narrower than
DDB's general documentation: tutorials, concepts, deployment guides, and
operator documentation remain in `ddb/docs` and the project's GitBook.

## Published routes

- `/`: API documentation landing page;
- `/schema/`: descriptor-driven Protobuf messages, fields, enums, oneofs,
  ProtoJSON representations, and operation cross-links;
- `/openapi/`: standalone Redoc CE view of HTTP/ProtoJSON operations;
- `/asyncapi/`: standalone AsyncAPI view of replayable event streams;
- `/sdk/`: Rust, TypeScript, and Python SDK references sourced from each
  package README;
- `/specs/`: versioned machine-readable API files, Protobuf sources, descriptor
  set, schema reference, checksums, and build metadata.

The deployed GitHub Pages base path is `/DDB/`. The build uses Docusaurus's
`pathname://` escape hatch only for generated static references such as Redoc
and AsyncAPI; tests still resolve those links against the final artifact.

## Brand asset

`static/img/ddb-logo.png` is a byte-for-byte copy of the
[public landing-page logo](https://usc-nsl-ddb.github.io/landing-page/ddb-logo.png).
The portal uses this file for both the navbar logo and favicon. Update both
deployments together when the DDB logo changes.

## Sources of truth

The site is generated from these API sources:

1. `ddb/proto/ddb/api/v2/*.proto` defines names, field numbers, types, comments,
   enum symbols, and oneofs.
2. `ddb/api-types/descriptor/ddb_api_v2_descriptor.bin` is the checked
   descriptor set used for structural reflection.
3. `ddb/docs/api/generated/operation-registry-v2.json` joins schema types to
   HTTP paths, permissions, status codes, and streaming behavior.
4. Generated OpenAPI and AsyncAPI documents define the HTTP and event-stream
   views.

`scripts/generate-portal-content.mjs` decodes the descriptor, parses Protobuf
source comments, joins the operation registry, and rejects undocumented public
messages or fields, missing type references, and descriptor/source drift. It
also emits `schema-reference-v2.json` for tools that need the same enriched
model without scraping HTML.

Generated files under `.generated/`, `.docusaurus/`, and `dist/` are
ephemeral and must not be committed.

## Install and verify

Node.js 24 LTS with npm 11.17 is required. `.node-version` pins the CI release.

    npm ci --ignore-scripts
    npm ci --ignore-scripts --prefix redoc-runtime
    npm ci --ignore-scripts --prefix asyncapi-runtime
    npm run check
    npm run build
    npm test

`npm run check` validates the checked OpenAPI and AsyncAPI documents.
`npm run build` deterministically prepares every generated input, builds both
standalone viewers, publishes the raw artifacts, and then creates the
Docusaurus site. `npm test` verifies the portal routes, every generated schema
page, SDK pages, byte-identical source artifacts, schema relationships,
checksums, provenance, base-path-safe local links, and self-hosted executable
assets.

To inspect the production build:

    npm run serve -- --host 127.0.0.1 --port 8080

Open `http://127.0.0.1:8080/DDB/`.

For theme or page development, run:

    npm start

This prepares schema and static references once, then starts the Docusaurus
development server. Restart it after changing a Protobuf file, generated API
specification, or SDK README so those inputs are regenerated.

## Dependency isolation

The portal uses React 19 through Docusaurus. Redoc's browser runtime and the
AsyncAPI renderer use separate lockfiles and installation roots so their
React/tooling dependencies do not affect the portal bundle. The build replaces
Redoc's generated CDN tag with the pinned local bundle and rejects runtime
version drift. The AsyncAPI renderer is also installed separately and pinned by
its own lockfile.

## Deployment

`.github/workflows/api-docs-pages.yml` validates relevant pull requests and
pushes. It uploads and deploys `dist/` only from the repository's current
default branch, so feature branches cannot replace the public site. The Pages
artifact includes `.nojekyll`, uses the configured `/DDB/` base path, and
contains no runtime dependency on a third-party script CDN.
