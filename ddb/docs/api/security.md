# DDB API security and deployment guide

DDB controls live debugger processes. Source reads, expression evaluation,
memory access, raw commands, signals, and shutdown can expose data or execute
code with the DDB process's operating-system privileges. Treat API access as
privileged debugger access, not as access to an ordinary read-only web service.

## Safe defaults and remote deployment

The HTTP listener binds to `127.0.0.1` by default. With no token file, public
health/server metadata remains available but every protected v2 method is
locked. Frozen v1 and unversioned legacy routes are available only on a
loopback listener because they predate v2 authentication.

A non-loopback bind fails at startup unless one of these policies is explicit:

- production: `api_auth_token_file` is configured and
  `api_tls_terminated_by_trusted_proxy` is `true`; or
- development only: `api_insecure_allow_remote` is `true`.

The trusted-proxy flag is an operator assertion, not automatic proxy
discovery. Firewall the DDB port so only the proxy can connect, protect the
proxy-to-DDB hop, strip untrusted forwarding headers, enforce TLS at the proxy,
and configure request/header/idle timeouts there. DDB never infers trust from
`Forwarded` or `X-Forwarded-*` headers.

Example production boundary:

```yaml
Conf:
  api_server_bind: 0.0.0.0
  api_server_port: 5000
  api_auth_token_file: /run/secrets/ddb-api-tokens.json
  api_tls_terminated_by_trusted_proxy: true
  api_cors_allowed_origins:
    - https://debug.example.com
  api_max_concurrent_requests: 128
  api_requests_per_second: 1000
  ApiLimits:
    state_replay_events: 10000
    state_replay_bytes: 33554432
    state_replay_retention_millis: 300000
    max_subscribers: 20
```

`api_insecure_allow_remote` bypasses only the remote transport check; it does
not enable unauthenticated v2. `api_insecure_allow_unauthenticated_v2` is a
separate local-development switch. Enabling both switches creates a fully
insecure remote listener and must never be used with real debuggees.

The optional gRPC preview always binds to `127.0.0.1`, uses the same token
grants, and is not a remote-exposure shortcut.

## Token file and scopes

The token document is JSON:

```json
{
  "tokens": [
    {"token": "at-least-32-random-bytes........", "scope": "read"},
    {"token": "another-random-secret...........", "scope": "control"},
    {"token": "separate-admin-secret............", "scope": "admin"}
  ]
}
```

The file must be a regular file, at most 64 KiB, contain 1–64 unique tokens,
and on Unix have no group/other permission bits (`0600` is recommended). Tokens
must contain 32–512 bytes. DDB retains SHA-256 digests and compares them in
constant time; logs use only a truncated digest-derived principal ID. Restart
DDB to rotate grants. Never place credentials in the main YAML, command line,
URL, logs, examples, or benchmark artifacts.

Scopes are hierarchical:

- unauthenticated: `GetServerInfo`, `GetHealth`, and `GetReadiness` only;
- `READ`: capabilities, topology, frames, variables, registers, sources,
  breakpoints, operations, registered extension schemas/state, and streams;
- `CONTROL`: all reads plus execution, breakpoint mutation, evaluation, raw
  commands, distributed operations, extension actions requiring control, and
  memory reads; and
- `ADMIN`: all scopes plus shutdown and extension actions requiring admin.

Memory is intentionally `CONTROL`, not `READ`, because arbitrary process
memory is often more sensitive than ordinary debugger state. Evaluation and
raw commands can have side effects even when their names sound observational.

## Browser and admission policy

An empty CORS list rejects every request carrying an `Origin` header. Entries
must be exact `http` or `https` origins; wildcards, paths, queries, fragments,
whitespace, and duplicates are rejected at startup. CORS protects browsers—it
does not authenticate scripts or prevent direct network calls.

DDB rejects all compressed request bodies, including gzip. This avoids a
decompression amplification surface; send ordinary bounded JSON. Unary v2
requests have a 4 MiB decoded body limit. Listener-wide token-bucket rate and
non-queuing concurrency limits fail with typed `RESOURCE_EXHAUSTED` responses
and `Retry-After: 1`. Event/output subscribers, replay, pages, memory, sources,
variables, operations, and extension data have separate advertised bounds.
Configured `Conf.ApiLimits` values are validated before any journal, operation
store, or broadcast queue is allocated. See
[`operations.md`](operations.md#capacity-and-client-diagnostics) for defaults
and hard operator ceilings.

For internet-facing deployments the trusted proxy must additionally enforce
header-read, body-read, total-request, and idle timeouts. Long-lived v2 event
streams must be exempted only from the total-response timeout and retain an
idle/connection policy appropriate for the deployment.

## Threat model

| Surface | Main risk | Enforced boundary |
|---|---|---|
| Source | Reading host files | A source path must first be reported by an attached debugger; clients receive an opaque per-instance reference; only regular UTF-8 files up to 2 MiB and bounded line windows are read. Run DDB under a least-privileged OS account. |
| Memory/registers | Secrets and process state | Memory requires `CONTROL`, a single stopped target, a bounded chunk, and an execution-revision check. |
| Evaluation/raw command | Side effects and backend escape | `CONTROL`, explicit capability, bounded request/result, normal operation/idempotency tracking. Raw result semantics are unstable by design. |
| Breakpoints/execution/signals | Debuggee mutation | `CONTROL`, canonical target resolution, preconditions, idempotency, asynchronous operation records, per-target outcomes. |
| Extensions | Untrusted in-process code and dynamic data | Only trusted provider crates are linked; descriptor/schema collisions fail startup; payloads are bounded; dynamic scopes are checked; provider messages are sanitized; failure cannot suppress core state. |
| Replay/output | Memory exhaustion or slow consumer | Independent bounded lanes, count/byte/age retention, subscriber caps, non-blocking per-subscriber pumps, explicit replay/output gaps with known dropped-byte counts. |
| Reflection/discovery | Contract enumeration | Standard gRPC reflection and health require `READ`; the minimal DDB health/readiness/server-info methods are deliberately public. |
| Shutdown | Remote denial of service | `ADMIN`, broadcast target, idempotency, operation record, graceful stream closure. |

Opaque public IDs prevent accidental backend-ID coupling; they are not an
authorization boundary. Scope checks occur before application methods, and
target resolution still validates current server-instance state.

## Logging, traces, and incident handling

Default API logs and traces contain only method/route, status, duration,
required scope, sanitized principal reference, operation/request IDs, stable
error code, counts, and byte sizes. They do not contain authorization headers,
tokens, raw commands, expressions, source/memory content, debugger output, or
extension payloads. Privileged authorization decisions use the
`ddb::api::audit` target. Operation history is bounded debugger state, not a
durable compliance audit log; export and retain audit records according to the
deployment's policy.

If a token may be compromised, block access at the proxy, replace the token
file, restart DDB, and review privileged authorization plus operation records.
Report security defects privately through the repository maintainers' current
security-reporting channel rather than a public issue containing exploit or
debuggee details.
