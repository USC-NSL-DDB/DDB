# Call-depth methodology

All 14 processes run on one physical native-k3s host. A one-shot request reaches
the selected handler breakpoint. DDB then interrupts every other process and
timing begins only after all 14 have a nonzero kernel `TracerPid` and tracing-
stop state.

Latency starts at DDB's timestamped `received cmd` record and ends at its final
token-matched timestamped MI output. Each reported DBT is rejected if it causes
another stop event or returns a boundary count different from the requested
depth.

The first same-pause DBT primes the command path and is excluded from all
reported results and terminal progress. It remains only in the raw per-depth
CSV for auditability. The results table uses repeats 2–30. P95 and P99 use
`sorted_values[int(p * (n - 1))]`.
