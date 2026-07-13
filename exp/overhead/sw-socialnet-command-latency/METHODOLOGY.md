# Command-latency methodology

The application remains distributed across the full Chameleon worker cluster.
DDB attaches to all 14 processes through their debugger sidecars, broadcasts
its global interrupt command, and begins timing only after every process is in
kernel tracing-stop state. Each debugger loads the upstream `extension.py`
before the ServiceWeaver runtime extension; the DDB Rust source is unchanged.

Latency starts at DDB's timestamped `received cmd` record and ends at its final
token-matched timestamped MI output. One warm-up pass is excluded. The reported
table contains 30 measured batches. Each batch submits one tokened DBT for every
discovered thread before waiting for responses, then waits for the entire batch
before starting the next one. Thread submission is interleaved across DDB
sessions to expose cross-process concurrency. All batches run under one global
pause with no continue between them. Any batch that creates a new stop event
fails validation.

P95 and P99 use `sorted_values[int(p * (n - 1))]`.
