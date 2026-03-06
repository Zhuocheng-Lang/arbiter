# arbiter

[English](README.md) | [简体中文](README_ZH_CN.md)

> A non-intrusive process priority manager for Linux, designed as a companion to modern `sched_ext` (scx) schedulers.

Arbiter sits alongside your scx scheduler and applies per-process `nice`, `ionice`, `cgroup`, and `oom_score_adj` tuning based on community-maintained rule sets — fully compatible with the [Ananicy-cpp](https://gitlab.com/ananicy-cpp/ananicy-cpp) rule format, with extensions for scx-aware environments.

## Why arbiter?

Modern scx schedulers (`scx_lavd`, `scx_bpfland`, `scx_rustland`, etc.) take over CPU scheduling entirely. This means classic CFS-based priority tuning tools lose much of their effect — and most of them don't even know scx is running.

Arbiter is designed with scx as a first-class consideration:

|                            | Ananicy-cpp           | arbiter                               |
| -------------------------- | --------------------- | ------------------------------------- |
| Rule format                | Own format            | Compatible with Ananicy-cpp rules     |
| Process detection          | polls / Netlink / BPF | Netlink `PROC_EVENT_EXEC` (~1 ms)     |
| scx awareness              | ✗                     | ✓ Detects active scx scheduler        |
| `ionice` / `oom_score_adj` | ✓                     | ✓                                     |
| `nice` tuning              | ✓                     | ✓ (with scx weight propagation notes) |
| cgroup placement           | systemd slice         | ✓ + scx_layered-aware mode            |
| Runtime                    | C++                   | Rust                                  |
| Coexistence with scx       | Unaware               | Designed for it                       |

Arbiter does **not** replace your scx scheduler. It adjusts the process attributes that scx schedulers expose to userspace and use as hints — `p->scx.weight` (derived from nice), IO priority, and cgroup hierarchy — without interfering with scheduling decisions.

## Features

- **Ananicy-cpp rule compatibility** — Drop in your existing `.rules` and `.types` files; `.cgroups` files are currently reported and ignored
- **Event-driven process detection** — Near-instant response to new processes via netlink connector
- **scx-aware behavior** — Detects the active scheduler and adapts strategy accordingly
- **`nice` / `ionice` / `oom_score_adj`** — Full support for all standard priority axes
- **cgroup v2 placement** — Integrates with systemd slices; enhanced mode for `scx_layered`
- **Extended rule format** — Optional arbiter-specific fields for finer control, fully backwards-compatible
- **Low overhead** — Written in Rust; event-driven architecture with negligible CPU usage at idle

## Architecture

The codebase is organized around three explicit layers:

- **app** — command execution and startup orchestration
- **rules** — rule loading, diagnostics, resolution, and matching
- **platform/linux** — Linux-specific event streaming and scx scheduler detection

The remaining top-level modules are intentionally narrow:

- **daemon** owns runtime orchestration and signal handling
- **applier** owns scheduler-aware application of resolved rules
- **cli** and **config** define input models rather than business logic

See [docs/architecture.md](docs/architecture.md) for the developer-facing layout and runtime flow.

## Requirements

- Linux kernel ≥ 6.12 (for `sched_ext` support; arbiter also works without scx)
- `CAP_NET_ADMIN` and `CAP_SYS_NICE` (or run as root)
- cgroup v2 unified hierarchy

## Relationship to Ananicy-cpp

Arbiter is designed as a **replacement** for Ananicy-cpp in scx environments, not a companion. Running both simultaneously will cause conflicts (both will renice the same processes). If you use arbiter, disable ananicy-cpp.

The Ananicy-cpp community rule files (from [ananicy-rules](https://github.com/CachyOS/ananicy-rules) and similar repos) are directly usable with arbiter — this is an explicit design goal.

## License

MIT
