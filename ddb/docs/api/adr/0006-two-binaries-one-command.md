# ADR 0006: Keep two binaries behind a one-command debugger workflow

Status: accepted

Date: 2026-08-15

## Context

DDB is becoming an independently deployable debugger backend whose public API
must support first-party and community frontends. The official terminal UI
should also feel like one debugger: users should not need a second terminal,
choose a port, create credentials, or manually coordinate startup.

Embedding the TUI in DDB would make installation simple, but it would couple
terminal dependencies and release cadence to the backend and weaken the public
API as the only supported frontend boundary. Requiring users to start both
processes manually would preserve separation at the cost of ordinary usability,
including for distributed configurations where one local DDB still coordinates
many targets.

## Decision

DDB and `ddb-tui` remain separate executable projects and communicate only
through the published DDB API.

`ddb tui ...` is a thin dispatcher. It resolves a paired `ddb-tui`, passes the
absolute path of the current DDB executable, and replaces itself with the
frontend on Unix. `ddb-tui` is the only managed-process supervisor. Direct
`ddb-tui ...` and dispatched `ddb tui ...` therefore share one implementation.

For configured, launch, and attach modes, `ddb-tui` starts `ddb serve` on an
OS-assigned loopback port with per-run bearer credentials and an atomic,
versioned startup report. DDB alone parses debugger configuration and constructs
shortcut sessions. The TUI performs debugger reads and mutations exclusively
through `ddb-api-client`.

`connect` is an ownership choice, not a topology choice. It is used only when a
DDB lifecycle is intentionally external, such as a service manager, container,
shared team service, or automation. Local and distributed target topologies both
use the managed one-command workflow by default.

Quitting the TUI shuts down only the backend it owns. Generated launches kill
their debuggees; generated attaches detach. Configuration-driven sessions inherit
the global exit policy unless an optional per-session policy is specified.

## Consequences

The two binaries can be developed and deployed independently, while paired
packages provide a one-command experience and sibling resolution without
`PATH`. Headless DDB and third-party frontends remain first-class.

Managed mode adds process supervision, a narrow launcher report, and temporary
credential handling. Those are lifecycle concerns, not a private debugger
protocol. No in-process shortcut, shared mutable state, shell-joined debuggee
command, or TUI import of DDB core/configuration types is permitted.

A managed backend normally shares the TUI lifetime. Persistence is not offered
until ownership transfer, durable private credentials, reconnect metadata,
rotation, and cleanup have a complete design. Distributed target hydration
continues after API readiness and remains visible through public state/events.

Release artifacts must pair the binaries, declare their API compatibility, and
smoke-test sibling discovery from an extracted archive. A project license must
be selected before an artifact can be called an official open-source release.
