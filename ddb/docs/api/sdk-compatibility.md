# DDB SDK compatibility

Status: preview. This table becomes a release support commitment when API v2
reaches general availability.

| Artifact | Version | Supported server API | Schema baseline | Status |
|---|---:|---|---|---|
| `ddb-api-types` | `0.1.x` | `ddb.api.v2` | `2.0.0-draft.3` | Preview contract/types |
| `ddb-api-client` | `0.1.x` | HTTP/ProtoJSON API v2 | `2.0.0-draft.3` | Preview Rust SDK |
| `ddb-api-grpc` | `0.1.x` | opt-in gRPC API v2 | `2.0.0-draft.3` | Transport preview |
| `ddb-api-extension` | `0.1.x` | extension descriptors/actions for API v2 | `2.0.0-draft.3` | Preview extension authoring SDK |
| `ddb-api-conformance` | `0.1.x` | HTTP/ProtoJSON API v2 | `2.0.0-draft.3` | Black-box server verifier |
| `@ddb-debugger/api-client` | `0.1.x` | HTTP/ProtoJSON API v2 | `2.0.0-draft.3` | Preview TypeScript SDK; Node 18+ and modern browsers |
| `ddb-api-client` (Python) | `0.1.x` | HTTP/ProtoJSON API v2 | `2.0.0-draft.3` | Preview typed Python SDK; Python 3.11+ |
| DDB API v1 | server-bundled | `/api/v1` | frozen JSON contract | Compatibility surface |

All three language SDKs negotiate `GetServerInfo` and `GetCapabilities` and
reject a server that does not advertise API v2. Minor `0.1.x` releases may add
methods, messages, enum values, and capabilities according to
[`compatibility.md`](compatibility.md), but do not silently reinterpret an
existing field. An unknown capability is ignored and an unsupported operation
returns the typed `DDB_ERROR_CODE_UNSUPPORTED` error.

The v1 contract is intentionally separate. Numeric v1 IDs are never inferred
from opaque v2 IDs. A frontend that enables a migration fallback must label v1
in diagnostics and keep its adapter isolated from the v2 SDK.

## Release dry run

From `ddb/`, run:

```bash
./tools/check-api-release.sh
```

The command regenerates contracts in check mode; tests and lints the public
Rust, TypeScript, and Python SDKs; packages every publishable artifact without
publishing; then packages each a second time and requires byte-for-byte
reproducibility. Local crates.io patching is used only because dependent preview
crates cannot resolve `ddb-api-types` until it has been published; packaged
manifests retain normal version dependencies.

Rust publication order is `ddb-api-types`, `ddb-api-grpc`, `ddb-api-client`,
`ddb-api-extension`, then `ddb-api-conformance`. The extension crate depends
only on the public types package, so it may also be published immediately after
`ddb-api-types` when release tooling permits. npm and Python artifacts have no
cross-registry ordering dependency. Publishing is never performed by the check
script.
