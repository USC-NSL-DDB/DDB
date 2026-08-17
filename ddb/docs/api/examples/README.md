# DDB v2 HTTP examples

These examples use only the stable public HTTP/ProtoJSON binding.

Start DDB with its API listener enabled, then set:

    export DDB_API_URL=http://127.0.0.1:8080
    export DDB_API_TOKEN='<read-scope bearer token>'

Run the curl hydration/reconnect flow with:

    bash docs/api/examples/curl-v2.sh

`browser-state-client.ts` is dependency-free TypeScript built on `fetch` and
Web Streams. Import `connectDdbState` from a browser application served from
the same origin (or through an operator-configured reverse proxy), or from a
modern JavaScript runtime. It
hydrates a snapshot, subscribes strictly after the returned cursor, ignores
blank heartbeat lines, and reports `REQUIRED_RESYNC` to its caller.

These low-level files illustrate the wire binding. Production frontends should
use the maintained [TypeScript SDK](../../../sdk/typescript/README.md) or
[Python SDK](../../../sdk/python/README.md), whose `examples/` directories cover
inspection, replay-aware events, breakpoint lifecycle, and DDB distributed
backtrace. The SDKs enforce bounds, idempotency, reconnect, and shutdown policy
that these short transport demonstrations intentionally omit.
