# Arbiter 架构

Arbiter 的结构围绕三个层次，以便 CLI 编排、规则语义和 Linux 集成可以独立演进。

## 模块布局

```text
src/
  app/              # CLI 命令执行和启动编排
  platform/linux/   # 与 Linux 相关的调度器检测和 proc 事件
  rules/            # 规则加载、验证、解析、匹配
  applier.rs        # 高级规则应用策略
  cli.rs            # 仅包含 clap 模型
  config.rs         # 持久化配置模型和加载
  daemon.rs         # 作为长运行服务的运行时编排
  lib.rs            # 公共 crate 接口
  main.rs           # 精简的二进制入口点
```

## 职责

- app：将解析的 CLI 输入转换为具体的应用流程，例如 `check`、`status` 和 `explain`
- daemon：负责生命周期管理、信号处理、工作队列和组件连接
- rules：负责文件解析、诊断、类型解析和匹配语义
- applier：将已解析的规则转化为具体的系统更改，同时保持对调度器的感知
- platform/linux：隔离面向内核的代码，如 `CN_PROC` netlink 处理和 scx 调度器探测

## 运行时流程

1. `main` 初始化 tracing 并委托给 `app::run`。
2. `app::run` 加载配置并调度所请求的命令。
3. `daemon` 从 `rules::RuleSet` 加载规则，通过 `platform::linux` 检测活动调度器，并启动进程事件流。
4. 每个 exec 事件变成一个从 `/proc` 构建的 `rules::ProcessContext`。
5. `rules::Matcher` 选择第一个匹配的已解析规则。
6. `Applier` 通过 Linux 接口（例如优先级 syscall 和 cgroup 控制文件）应用生成的提示。

## 公共边界

该 crate 现在有意将其公共接口保持得很小：

- `cli` 定义命令行模型
- `app` 公开命令执行
- `rules` 公开规则解析和匹配原语
- `platform::linux` 公开面向内核的事件和调度器接口

诸如原始 netlink 线协议解析等内部细节仍保留在 `platform/linux/events.rs` 中，不再混入运行时编排代码。
