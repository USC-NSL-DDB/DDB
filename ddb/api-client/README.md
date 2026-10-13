# ddb-api-client

Typed Rust client for DDB API v2 frontends.

The client handles ProtoJSON framing, authentication, deadlines, idempotency,
bounded pagination, operation polling, and reconnecting snapshot/event and
output workflows. It depends only on the public `ddb-api-types` crate and
transport libraries, not on DDB core internals.

```no_run
use ddb_api_client::{ClientConfig, DdbClient, ProjectedStateSyncItem, StateSyncOptions};

#[tokio::main]
async fn main() -> ddb_api_client::Result<()> {
    let config = ClientConfig::new("http://127.0.0.1:5000")
        .with_bearer_token(std::env::var("DDB_API_TOKEN").unwrap());
    let client = DdbClient::new(config)?;
    let (_server, capabilities) = client.handshake().await?;
    println!("DDB schema {}", capabilities.schema_version);

    let mut state = client.projected_state_sync(StateSyncOptions::default())?;
    if let ProjectedStateSyncItem::Snapshot = state.next().await? {
        println!("{} sessions", state.projection().unwrap().sessions().len());
    }
    Ok(())
}
```

HTTP/ProtoJSON is the required baseline transport. The separate native gRPC
binding remains preview-only under the transport policy in
[`ADR 0005`](../docs/api/adr/0005-transport-policy.md). Clients must select
from advertised endpoints and must not require gRPC.

## Request metadata tracing

Call `client.subscribe_request_traces()` before `handshake()` to receive optional
`RequestTrace` records. Client clones share attempt IDs and a bounded 256-record
broadcast channel. Consume records with `recv()` and handle `Lagged(count)`;
slow readers never hold up HTTP requests. No tracing channel is allocated until
the first subscription, and attempts are not recorded without a subscriber.

Records contain the canonical service/operation name, a per-attempt ID, and
`Started`, `Response(status)`, `TransportFailed`, or `Cancelled`. Each retry has
its own ID. `Display` formats a POST path relative to the canonical RPC root,
never the configured endpoint. Tokens, headers, endpoint prefixes, queries,
request/response bodies, resource IDs and raw error text are excluded.

Response records describe received HTTP headers. They do not certify response
decoding or debugger operation success. Stream setup is traced, but stream
payloads and later stream failures are not. Use the typed operation receipts
and stream errors for those outcomes.
