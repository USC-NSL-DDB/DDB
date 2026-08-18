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
