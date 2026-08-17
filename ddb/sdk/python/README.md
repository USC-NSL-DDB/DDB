# `ddb-api-client`

Typed Python 3.11+ client for the public DDB API v2 HTTP/ProtoJSON binding. It
uses only the standard library at runtime and can be embedded in CLIs, IDE
plugins, notebooks, and alternative debugger frontends.

```python
import os

from ddb_api import DdbClient

with DdbClient(
    "http://127.0.0.1:5000",
    bearer_token=os.environ.get("DDB_API_TOKEN"),
) as client:
    server, capabilities = client.handshake()
    snapshot = client.get_snapshot({
        "sections": ["SNAPSHOT_SECTION_TOPOLOGY", "SNAPSHOT_SECTION_EXECUTION"],
    })["snapshot"]
    print(server["version"], capabilities["schemaVersion"], snapshot["threads"])
```

`call()` covers every generated public unary method. `collect()` follows
bounded cursor pages; `stream()` parses bounded NDJSON; `state_sync()` performs
snapshot-plus-replay and rehydrates after typed replay gaps. State and output
subscriptions reconnect with bounded exponential backoff. Context-manager exit
or `close()` closes active responses, interrupts retry waits, and prevents new
requests.

Mutation contexts receive a UUID idempotency key when omitted, and all requests
receive an RFC 3339 deadline. Typed API errors retain the full safe `DdbError`
dictionary. Only an untyped HTTP 404 reports `is_api_version_unavailable()`;
typed not-found, authentication, malformed-response, and transport failures do
not permit a silent downgrade.

The `ddb_api.generated.types` module contains forward-compatible `TypedDict`
contracts generated directly from Protobuf. Regenerate checked-in bindings from
the Rust workspace with:

```bash
cargo run -p ddb-api-codegen -- generate
```
