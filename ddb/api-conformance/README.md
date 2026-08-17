# ddb-api-conformance

Black-box conformance runner for DDB API v2 servers. The runner uses only
`ddb-api-client` and public contract types; it never imports DDB core code or
reads backend state directly.

The default profile is non-mutating:

```bash
cargo run -p ddb-api-conformance -- \
  --endpoint http://127.0.0.1:5000 \
  --output text
```

Use `DDB_API_TOKEN` for authenticated deployments. JSON output is suitable for
CI evidence:

```bash
DDB_API_TOKEN="$(cat /secure/path/read-token)" \
  cargo run -p ddb-api-conformance -- \
  --endpoint http://127.0.0.1:5000 \
  --output json
```

The read-only profile checks discovery and effective limits, health/readiness,
all snapshot sections and topology references, bounded pagination, state and
output stream admission, and stopped-thread frame/scope/variable/register/source
inspection when a stopped thread exists. Missing runtime-dependent inspection
is reported as skipped, never as silently passed.

The `mock` profile is intentionally mutating and must be used only with a
disposable DDB Mock fixture and a CONTROL credential:

```bash
DDB_API_TOKEN="$(cat /secure/path/control-token)" \
  cargo run -p ddb-api-conformance -- \
  --endpoint http://127.0.0.1:5000 \
  --profile mock \
  --output json
```

It additionally verifies mutation idempotency, typed evaluation and memory,
independent output delivery, breakpoint create/delete, distributed backtrace,
state-event delivery, and observable step-over execution-line movement.
The runner exits with status 2 when any check fails and never includes bearer
credentials, source text, memory bytes, expressions, or raw command text in its
report.
