# `@ddb-debugger/api-client`

Dependency-free TypeScript client for the public DDB API v2 HTTP/ProtoJSON
binding. It works with Node.js 18+ and modern browser runtimes that provide
`fetch`, `ReadableStream`, `AbortController`, and Web Crypto.

```ts
import { DdbClient, ExecutionActionValues } from "@ddb-debugger/api-client";

const client = new DdbClient({
  endpoint: "http://127.0.0.1:5000",
  bearerToken: process.env.DDB_API_TOKEN,
});

const { capabilities } = await client.handshake();
const snapshot = await client.call("DebuggerService.GetSnapshot", {
  sections: ["SNAPSHOT_SECTION_TOPOLOGY", "SNAPSHOT_SECTION_EXECUTION"],
});

const stopped = snapshot.snapshot?.threads?.find(
  (thread) => thread.state === "THREAD_STATE_STOPPED",
);
if (stopped?.threadId) {
  const admission = await client.call("DebuggerControlService.Execute", {
    target: { thread: { threadId: stopped.threadId } },
    action: ExecutionActionValues.EXECUTION_ACTION_NEXT,
  });
  await client.waitOperation(admission.operation?.operationId ?? "");
}
console.log(capabilities.apiVersion);
client.close();
```

`call` and `stream` cover every generated public method. `collect` performs
bounded cursor pagination. `stateSync` implements snapshot-plus-replay and
rehydrates after a typed replay gap. `subscribeStateEvents` and
`subscribeOutput` reconnect with bounded exponential backoff. `close()` aborts
active calls and streams.

Mutation request contexts receive a UUID idempotency key when omitted. Every
request receives an RFC 3339 deadline unless the caller supplied one. Payload,
collection, and NDJSON-line limits are enforced before unbounded allocation.
Only an untyped HTTP 404 reports `isApiVersionUnavailable()`: authentication,
typed resource-not-found errors, malformed responses, and connectivity errors
never authorize a silent v1 downgrade.

Generated files under `src/generated` come from the canonical Protobuf
descriptor. Regenerate them from the DDB Rust workspace with:

```bash
cargo run -p ddb-api-codegen -- generate
```
