# arbiter

[English](README.md) | [简体中文](README_ZH_CN.md)

> A non-intrusive Linux process priority manager designed as a companion tool for modern `sched_ext` (scx) schedulers.

Arbiter runs in parallel with scx schedulers, adjusting `nice`, `ionice`, `cgroup`, and `oom_score_adj` for each process based on a community-maintained rule set. It is fully compatible with the [Ananicy-cpp](https://gitlab.com/ananicy-cpp/ananicy-cpp) rule format and includes extensions to support scx environments.

## Why Arbiter?

### tl;dr

Modern scx schedulers (such as `scx_lavd`, `scx_bpfland`, etc.) take full control of CPU scheduling. Traditional CFS priority adjustment tools (like the `Ananicy` series) have mostly lost their effectiveness. However, `Arbiter` focuses on adjusting peripheral parameters—`nice`, `ionice`, `cgroup`, etc.—to assist the scx scheduler by modifying weights.

### Why traditional schedulers are becoming obsolete

The real issue with traditional schedulers isn't that they were never useful, but that they were born in an era where a specific premise held true: the kernel decides everything, and user space can only fine-tune parameters within an established order. CFS represents the most mature product of this paradigm. It attempts to use a unified fairness model to house all processes, letting desktop interactions, background services, compilation tasks, and gaming loads compete for the CPU under a single logic. This was an engineering triumph in the past; today, it feels more like a clever but rigid compromise.

Modern Linux workloads have split into contradictory demands: some need the lowest latency, some need maximum throughput, some want smooth foregrounds with quiet backgrounds, and others require the system to dynamically bias based on hardware topology and task behavior. Fixed kernel scheduling policies are no longer sufficient to cover these differences, leading to the birth of `sched_ext`. It doesn't just patch the old order; it turns "scheduling policy itself" into an object that can be replaced, experimented with, and evolved. When schedulers like `scx_lavd`, `scx_bpfland`, and `scx_layered` take over CPU decisions, traditional priority tools designed around CFS are destined to exit; the era hasn't denied their value, it has simply reclaimed the stage where they once performed.

### Why community rules based on `Ananicy` still matter

The replacement of a scheduler does not mean that experience is wiped away. The `Ananicy` rules accumulated by the community over the years represent a precious knowledge base that is difficult to rebuild: which processes are games, which are browsers, which are compilers, which workloads should aggressively fight for responsiveness, and which should step back to yield resources to the foreground. These judgments are not the "private goods" of any single scheduler, but rather systemic experience distilled from long-term practice.

In the scx era, these rules haven't become invalid; they have simply changed meaning. They no longer act as the final judge for the scheduler but provide a trusted source of signals: `nice` continues to influence weight, `ionice` and `oom_score_adj` still define trade-offs under system pressure, and `cgroup` remains an important boundary for organizing workload hierarchies. In other words, what the old rules preserve is not outdated techniques, but the ability to judge process identity and resource intent. What needs updating is not the community knowledge itself, but the way it is integrated into the modern scheduling ecosystem.

### The Role of Arbiter

Arbiter's purpose is to build a practical bridge across this fault line. It does not attempt to seize scheduling power back from scx, nor does it pretend to be another scheduler. Instead, it translates the community rules' judgments—about who a process is and how much responsiveness it should seek—into signal hints that scx still reads and respects. `nice` is not for the sake of nostalgia for CFS, but to let the scx scheduler derive weights; `ionice` and `oom_score_adj` are not historical relics, but effective languages for trade-offs during IO and memory pressure; `cgroup` continues to carry the responsibility of organizing workload boundaries and expressing structural intent to layered schedulers.

Therefore, Arbiter is neither a patch for the old era nor a replacement for new schedulers. It acts as a disciplined interpreter: upstream, it inherits the process knowledge accumulated by the Ananicy community; downstream, it interfaces with the parameter layer exposed by scx to user space. Scheduling policies have changed, but judgments about workload identity remain worth preserving. Arbiter's job is to ensure these judgments remain effective in the scx era in a more honest and modern way.

## Features

- **Inherit existing community rule assets** — Existing `.rules` and `.types` files can be used directly; `.cgroups` files are currently prompted and ignored.
- **Event-driven, not periodic polling** — Captures new process events via netlink connectors to complete matching and application with near-instant speed.
- **scx-aware policy switching** — Detects the currently active scx scheduler and selects a more appropriate application path accordingly.
- **Full coverage of standard priority signals** — Supports `nice`, `ionice`, and `oom_score_adj`, converting rule judgments into hints the system still consumes.
- **cgroup v2 oriented workload placement** — Can place processes into specified hierarchies and provides adaptation paths for `scx_layered`.
- **Backward-compatible extensibility** — While maintaining compatibility with existing rule formats, it allows the use of Arbiter's own additional fields for finer-grained control.
- **Low-intrusion, low idle cost** — Written in Rust and centered on an event-driven model, consuming almost no extra CPU when idle.

## Requirements

- Linux Kernel ≥ 6.12 (supporting `sched_ext`; arbiter can also work without scx).
- `CAP_NET_ADMIN` and `CAP_SYS_NICE` permissions (or run as root).
- cgroup v2 unified hierarchy.

## Relationship with Ananicy-cpp

In an scx environment, Arbiter is positioned as a **replacement** for Ananicy-cpp, not a companion to run alongside it. When both run simultaneously, they will attempt to repeatedly write `nice` and other attributes to the same processes, resulting in conflict rather than cooperation; if you decide to use Arbiter, you should disable Ananicy-cpp.

However, only the old execution method is being replaced, not the community-accumulated rule knowledge. Ananicy-cpp rule files from repositories like [ananicy-rules](https://github.com/CachyOS/ananicy-rules) are the upstream assets Arbiter aims to inherit: they continue to describe "who is who," while Arbiter ensures these judgments interface with the parameters still effective in the scx era. This is not a compatibility bonus, but a primary design goal pursued from the project's inception.

## License

MIT
