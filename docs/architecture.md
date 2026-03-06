# Arbiter Architecture

Arbiter is structured around three layers so that CLI orchestration, rule semantics, and Linux integration evolve independently.

## Module layout

```text
src/
  app/              # CLI command execution and startup orchestration
  platform/linux/   # Linux-specific scheduler detection and proc events
  rules/            # Rule loading, validation, resolution, matching
  applier.rs        # High-level rule application policy
  cli.rs            # clap models only
  config.rs         # persisted configuration model and loading
  daemon.rs         # runtime orchestration for the long-running service
  lib.rs            # public crate surface
  main.rs           # thin binary entrypoint
```

## Responsibilities

- app: turns parsed CLI input into concrete application flows such as `check`, `status`, and `explain`
- daemon: owns lifecycle, signal handling, worker queues, and component wiring
- rules: owns file parsing, diagnostics, type resolution, and matching semantics
- applier: translates a resolved rule into concrete system changes while staying scheduler-aware
- platform/linux: isolates kernel-facing code such as `CN_PROC` netlink handling and scx scheduler probing

## Runtime flow

1. `main` initializes tracing and delegates to `app::run`.
2. `app::run` loads configuration and dispatches the requested command.
3. `daemon` loads rules from `rules::RuleSet`, detects the active scheduler through `platform::linux`, and starts the process event stream.
4. Each exec event becomes a `rules::ProcessContext` built from `/proc`.
5. `rules::Matcher` selects the first matching resolved rule.
6. `Applier` applies the resulting hints through Linux interfaces such as priority syscalls and cgroup control files.

## Public boundaries

The crate now keeps its public surface intentionally small:

- `cli` defines command-line models
- `app` exposes command execution
- `rules` exposes rule parsing and matching primitives
- `platform::linux` exposes kernel-facing event and scheduler interfaces

Internal details such as raw netlink wire parsing remain inside `platform/linux/events.rs` and are no longer mixed into runtime orchestration code.
