# Arbiter 架构

Arbiter 的结构围绕三层展开，以便 CLI 编排、规则语义和 Linux 集成能够独立演进。

## 模块布局

```text
src/
  app/              # CLI 命令执行与启动编排
  platform/linux/   # 与 Linux 相关的调度器检测和 proc 事件处理
  rules/            # 规则加载、验证、解析与匹配
  applier.rs        # 高级规则应用逻辑
  cli.rs            # 仅包含 clap 命令模型
  config.rs         # 持久化配置模型与加载
  daemon.rs         # 长运行服务的运行时编排
  lib.rs            # 公开的 crate 接口
  main.rs           # 精简的二进制入口
```

## 职责

- app：将解析后的 CLI 输入转换为具体的应用流程，例如 `check`、`status` 和 `explain`
- daemon：负责生命周期管理、信号处理、工作队列和组件连接
- rules：负责文件解析、诊断、类型解析和匹配语义
- applier：将已解析的规则转化为具体的系统变更，同时保持对调度器的感知
- platform/linux：隔离面向内核的代码，例如 `CN_PROC` netlink 处理和 scx 调度器探测

## 规则语义

- 规则文件按逐行 JSON 对象读取，这样注释和差异都更容易维护。
- 每个目录都会先加载 `.types`，再加载 `.rules`，因此规则条目可以继承共享预设。
- 匹配采用“首次命中即生效”的策略；一旦某条规则匹配，后续规则就不再继续尝试。
- `.cgroups` 文件会被明确忽略，因为当前模型只支持 cgroup 放置和可选的 `cgroup_weight`。
- `ionice` 的语义只有在规则选择了 cgroup 目标时才会被转换为 cgroup v2 `io.weight`。
- 未识别字段会发出警告并忽略，以便解析器保持对未来扩展字段的前向兼容。

## 运行时流程

1. `main` 初始化 tracing 并委托给 `app::run`。
2. `app::run` 加载配置并调度所请求的命令。
3. `daemon` 从 `rules::RuleSet` 加载规则，通过 `platform::linux` 检测活动调度器，并启动进程事件流。
4. 每个 exec 事件变成一个从 `/proc` 构建的 `rules::ProcessContext`。
5. `rules::Matcher` 选择第一个匹配的已解析规则。
6. `Applier` 通过 Linux 接口（例如 `nice` / `oom_score_adj` 写入和 cgroup 控制文件）应用生成的调整；在适用时，`ionice` 会被转换为 cgroup v2 `io.weight`。

## 公共边界

该 crate 现在有意将其公共接口保持得很小：

- `cli` 定义命令行模型
- `app` 公开命令执行
- `rules` 公开规则解析和匹配原语
- `platform::linux` 公开面向内核的事件和调度器接口

诸如原始 netlink 协议解析等内部细节仍保留在 `platform/linux/events.rs` 中，不再混入运行时编排代码。
