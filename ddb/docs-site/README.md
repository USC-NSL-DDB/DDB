# DDB API reference portal

This directory builds the public API-reference site for developers creating DDB
frontends, SDKs, extensions, and integrations. It is deliberately narrower than
DDB's general documentation: tutorials, concepts, deployment guides, and
operator documentation remain in `ddb/docs` and the project's GitBook.

## Published routes

- `/`: Docusaurus API portal and reference chooser;
- `/schema/`: descriptor-driven Protobuf messages, fields, enums, oneofs,
  ProtoJSON representations, and operation cross-links;
- `/openapi/`: standalone Redoc CE view of HTTP/ProtoJSON operations;
- `/asyncapi/`: standalone AsyncAPI view of replayable event streams;
- `/sdk/`: Rust, TypeScript, and Python SDK references sourced from each
  package's canonical README;
- `/specs/`: stable machine-readable contracts, Protobuf sources, descriptor
  set, schema catalog, checksums, and build provenance.

The deployed GitHub Pages base path is `/DDB/`. The build uses Docusaurus's
`pathname://` escape hatch only for generated static references such as Redoc
and AsyncAPI; tests still resolve those links against the final artifact.

## Sources of truth

This site is a projection, never a competing API definition:

1. `ddb/proto/ddb/api/v2/*.proto` defines names, field numbers, types, comments,
   enum symbols, and oneofs.
2. `ddb/api-types/descriptor/ddb_api_v2_descriptor.bin` is the checked
   descriptor set used for structural reflection.
3. `ddb/docs/api/generated/operation-registry-v2.json` joins schema types to
   HTTP paths, permissions, status codes, and streaming behavior.
4. Generated OpenAPI and AsyncAPI contracts define their transport-specific
   views.

`scripts/generate-portal-content.mjs` decodes the descriptor, parses canonical
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

`npm run check` validates the checked OpenAPI and AsyncAPI contracts.
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

This prepares schema and static references once, then starts Docusaurus's
development server. Restart it after changing a Protobuf file, generated
contract, or SDK README so those inputs are regenerated.

## Dependency isolation

The portal uses React 19 through Docusaurus. Redoc's browser runtime and the
AsyncAPI renderer have separate lockfiles and installation roots because their
React/tooling dependency trees are unrelated and must not influence the portal
bundle. Redoc's generated CDN tag is replaced with the exactly pinned local
bundle; the build rejects renderer/runtime version drift. AsyncAPI's optional
Studio dependency is pinned independently to avoid registry instability in the
renderer toolchain.

## Deployment

`.github/workflows/api-docs-pages.yml` validates relevant pull requests and
pushes. It uploads and deploys `dist/` only from the repository's current
default branch, so feature branches cannot replace the public site. The Pages
artifact includes `.nojekyll`, uses the configured `/DDB/` base path, and
contains no runtime dependency on a third-party script CDN.
