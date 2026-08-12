# Call-depth methodology

All 14 processes are co-located on one physical native-k3s node. Other nodes
may remain joined to the cluster, but they host no measured application
process. A one-shot request reaches the selected handler breakpoint. DDB then
interrupts every other process, and timing begins only after all 14 have a
nonzero kernel `TracerPid` and tracing-stop state.

Latency starts at DDB's timestamped `received cmd` record and ends at its final
token-matched timestamped MI output. Each reported DBT is rejected if it causes
another stop event or returns an RPC-boundary count different from
`call_depth - 1`. Call depth includes the originating process, so one crossed
RPC boundary is call depth 2.

The measured request follows this synchronous component chain:

`Main -> Backend -> UserTimeline -> Relay1 -> Relay2 -> Relay3 -> Relay4 -> Relay5 -> Relay6 -> Storage`

Breakpoints select depths 2, 3, 4, 5, 6, and 10 along that chain. The relay
components are colocated with existing component groups and therefore preserve
the fixed 14-process deployment.

The first same-pause DBT primes the command path and is excluded from all
reported results and terminal progress. It remains only in the raw per-depth
CSV for auditability. The results table uses repeats 2–10. P95 and P99 use
`sorted_values[int(p * (n - 1))]`.
