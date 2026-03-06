# arbiter

> 一个非侵入性的 Linux 进程优先级管理器，设计为现代 `sched_ext`（scx）调度器的辅助工具。

Arbiter 与 scx 调度器并行运行，根据社区维护的规则集对每个进程的 `nice`、`ionice`、`cgroup` 和 `oom_score_adj` 进行调整——完全兼容 [Ananicy-cpp](https://gitlab.com/ananicy-cpp/ananicy-cpp) 的规则格式，并为支持 scx 环境添加了扩展。

## 为什么选择 arbiter？

现代 scx 调度器（如 `scx_lavd`、`scx_bpfland`、`scx_rustland` 等）完全接管了 CPU 调度。这意味着基于传统 CFS 的优先级调节工具大多失去了作用 —— 并且大多数工具甚至不知道 scx 正在运行。

Arbiter 的设计将 scx 作为首要考虑：

|                            | Ananicy-cpp          | arbiter                              |
| -------------------------- | -------------------- | ------------------------------------ |
| 规则格式                   | 自有格式             | 与 Ananicy-cpp 规则兼容              |
| 进程检测                   | 轮询 / Netlink / BPF | Netlink `PROC_EVENT_EXEC`（≈1 毫秒） |
| scx 感知                   | ✗                    | ✓ 检测活动的 scx 调度器              |
| `ionice` / `oom_score_adj` | ✓                    | ✓                                    |
| `nice` 调整                | ✓                    | ✓（带有 scx 权重传播说明）           |
| cgroup 放置                | systemd slice        | ✓ + scx_layered 感知模式             |
| 运行时                     | C++                  | Rust                                 |
| 与 scx 共存                | 不感知               | 专为其设计                           |

Arbiter **不**取代您的 scx 调度器。它只调整 scx 调度器暴露给用户空间并用作提示的进程属性——`p->scx.weight`（来源于 nice）、IO 优先级和 cgroup 层级——而不会干扰调度决策。

## 功能

- **兼容 Ananicy-cpp 规则** — 可直接使用现有的 `.rules` 和 `.types` 文件；`.cgroups` 文件当前会提示并忽略
- **事件驱动的进程检测** — 通过 netlink 连接器对新进程进行近乎即时响应
- **scx 感知行为** — 检测活动调度器并相应调整策略
- **支持 `nice` / `ionice` / `oom_score_adj`** — 全面支持所有标准优先级轴
- **cgroup v2 放置** — 与 systemd 切片集成；为 `scx_layered` 提供增强模式
- **扩展规则格式** — 可选的 arbiter 特定字段以实现更精细的控制，完全向后兼容
- **低开销** — 使用 Rust 编写；事件驱动架构在空闲时几乎不消耗 CPU

## 要求

- Linux 内核 ≥ 6.12（支持 `sched_ext`; arbiter 在没有 scx 的情况下也可工作）
- 拥有 `CAP_NET_ADMIN` 和 `CAP_SYS_NICE` 权限（或以 root 身份运行）
- cgroup v2 统一层级

## 与 Ananicy-cpp 的关系

Arbiter 旨在作为 scx 环境中 Ananicy-cpp 的**替代品**，而不是伴侣。两者同时运行会发生冲突（两者都会对同一进程重新设置 nice 值）。如果使用 arbiter，请禁用 ananicy-cpp。

Ananicy-cpp 社区规则文件（来自 [ananicy-rules](https://github.com/CachyOS/ananicy-rules) 等仓库）可以直接与 arbiter 一起使用——这是明确的设计目标。

## 许可

MIT
