# Arbiter 部署指南

本文档说明如何结合 CachyOS `ananicy-rules` 官方规则集，将 Arbiter 部署为系统级进程优先级管理守护进程。

---

## 目录

1. [前置要求](#前置要求)
2. [编译](#编译)
3. [安装规则集](#安装规则集)
4. [如何用 arbiter 运行 ananicy-rules](#如何用-arbiter-运行-ananicy-rules)
5. [配置](#配置)
6. [静态校验](#静态校验)
7. [运行守护进程](#运行守护进程)
8. [systemd 服务](#systemd-服务)
9. [热重载规则](#热重载规则)
10. [调试与验证](#调试与验证)
11. [兼容性说明](#兼容性说明)
12. [卸载](#卸载)

---

## 前置要求

| 要求        | 说明                                                                                                                                     |
| ----------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| Linux 内核  | ≥ 6.12（含 `sched_ext` BPF 框架；无 scx 调度器时也可正常工作）                                                                           |
| cgroup v2   | 统一层级（`/sys/fs/cgroup`），现代发行版默认已启用                                                                                       |
| 权限        | `CAP_NET_ADMIN`（读取 `CN_PROC` 进程事件）+ `CAP_SYS_NICE`（调整 `nice`；cgroup 写入仍需要相应的 cgroup 访问权限），或直接以 `root` 运行 |
| Rust 工具链 | 见 `rust-toolchain.toml`，建议使用 [uv](https://docs.astral.sh/uv/) 或 `rustup` 管理                                                     |

验证 cgroup v2：

```sh
stat -f /sys/fs/cgroup | grep Type
# 输出包含 cgroup2 即表示使用的是 v2
```

---

## 编译

```sh
# 在项目根目录
cargo build --release
# 产物路径：target/release/arbiter
```

可选：安装到系统路径：

```sh
sudo install -Dm755 target/release/arbiter /usr/local/bin/arbiter
```

---

## 安装规则集

### 使用 CachyOS 官方 ananicy-rules

```sh
# 克隆官方规则
git clone https://github.com/CachyOS/ananicy-rules /tmp/ananicy-rules

# 创建 arbiter 规则目录
sudo mkdir -p /etc/arbiter/rules.d

# 复制类型定义（必须先于 *.rules 加载）
sudo cp /tmp/ananicy-rules/00-types.types /etc/arbiter/rules.d/

# 复制进程规则（可按需挑选子目录）
sudo cp /tmp/ananicy-rules/00-default/*.rules /etc/arbiter/rules.d/ 2>/dev/null || true
sudo find /tmp/ananicy-rules/00-default -name '*.rules' \
    -exec sudo cp {} /etc/arbiter/rules.d/ \;
```

> **注意**：CachyOS 仓库中的 `00-cgroups.cgroups` 文件不会被 arbiter 加载（arbiter 只读取
> `*.types` 和 `*.rules`）。如需使用其中的 cgroup 策略，需手工迁移——参见[兼容性说明](#兼容性说明)。

### 使用项目内置规则

项目自带的 `rules/` 目录为最小参考集，可直接用于测试：

```sh
sudo mkdir -p /etc/arbiter/rules.d
sudo cp rules/*.types rules/*.rules /etc/arbiter/rules.d/
```

### 用户级规则（无需 root）

规则也可放到 XDG 用户目录，arbiter 会自动读取：

```sh
mkdir -p ~/.config/arbiter/rules.d
cp rules/*.types rules/*.rules ~/.config/arbiter/rules.d/
```

两个目录都配置时都会加载。对每个目录来说，arbiter 会先按字母序加载 `*.types`，再按字母序加载 `*.rules`；真正决定命中优先级的是规则顺序加上 first-match-wins，而不是额外的“覆盖”阶段。

---

## 如何用 arbiter 运行 ananicy-rules

`arbiter` 的目标是复用 `ananicy-rules` 这套社区维护的规则资产，而不是复刻 `ananicy-cpp` 的整个运行模型。对部署者来说，最重要的结论是：

- `arbiter` 会读取 `*.types` 和 `*.rules`
- `arbiter` 会对 `*.cgroups` 给出警告并忽略
- `arbiter` 依靠 `PROC_EVENT_EXEC` 事件驱动应用规则，而不是周期扫描
- `arbiter` 应该替代 `ananicy-cpp` 运行，而不是和它并行共存

### 实际会加载哪些文件

对每一个配置的规则目录，`arbiter` 都按以下顺序处理：

1. 按文件名字母序加载全部 `*.types`
2. 按文件名字母序加载全部 `*.rules`
3. 发现 `*.cgroups` 时只记录诊断并跳过

这保证了类型定义一定先于规则解析完成。同时也意味着规则顺序本身有语义：`arbiter` 使用 first-match-wins，较早命中的选择器会遮蔽后面的重复选择器。

### 在 arbiter 语境下，“运行 ananicy-rules”是什么意思

这里的“运行”实际上包含两层含义：

1. 继承 CachyOS / Ananicy-cpp 社区维护的进程分类与优先级经验
2. 通过 `arbiter` 的事件驱动守护进程把这些规则真正应用到进程上

在 `arbiter` 中，这条执行链路是：

1. 某个进程执行 `execve`
2. 内核发出 `PROC_EVENT_EXEC` netlink 事件
3. `arbiter` 等待 `exec_delay_ms`，让 `/proc/<pid>` 状态稳定
4. 匹配器读取 `comm`、exe basename、完整 exe 路径和 cmdline
5. 选出第一条命中的已解析规则
6. 写入 `nice`、`ionice`、`oom_score_adj`，以及可选的 cgroup 放置

当规则选择了 cgroup 目标时，`ionice` 会被转换为 cgroup v2 `io.weight`，而不是直接调用 `ioprio_set()`。对应关系大致为：`RT` → `800-1000`，`BE` → `100-800`，`Idle` → `1-50`；如果没有 cgroup 目标，则会跳过这一步。

因此它没有 `check_freq` 之类的扫描周期配置，因为它根本不是轮询式守护进程。

### 上游规则中哪些内容不会直接生效

最主要的不兼容点是 `00-cgroups.cgroups`。CachyOS 用这个文件表达 `CPUQuota` 之类的 cgroup 预设，而 `arbiter` 当前只支持把进程移入某个 cgroup，并可选写入 cgroup v2 的 `cpu.weight`。

这里应当把它理解为模型差异，而不是导入失败：

- `CPUQuota` 是硬上限
- `cgroup_weight` 是相对份额
- 两者语义不同，不能不经设计就自动互转

另外，`sched` 这类字段会为兼容性而被解析，但在应用阶段被忽略；`latency_nice` 也不会生效。

### 最小可行操作流程

如果你要通过 `arbiter` 运行 CachyOS `ananicy-rules`，建议按下面的顺序执行：

```sh
# 1. 安装上游规则资产
git clone https://github.com/CachyOS/ananicy-rules /tmp/ananicy-rules
sudo mkdir -p /etc/arbiter/rules.d
sudo cp /tmp/ananicy-rules/00-types.types /etc/arbiter/rules.d/
sudo find /tmp/ananicy-rules/00-default -name '*.rules' -exec sudo cp {} /etc/arbiter/rules.d/ \;

# 2. 校验语法与类型引用
arbiter check /etc/arbiter/rules.d

# 3. 在正式运行前先检查目标会命中哪条规则
arbiter explain firefox

# 4. 先用 dry-run 观察而不改写系统状态
sudo arbiter daemon --dry-run

# 5. 确认无误后再正式运行
sudo arbiter daemon
```

### 不要与 ananicy-cpp 同时运行

两个守护进程都会尝试修改相近的进程属性。把它们一起运行，结果通常是 `nice`、`ionice` 等状态出现 last-writer-wins 的相互覆盖。

如果决定切换到 `arbiter`，应先停用 `ananicy-cpp`：

```sh
sudo systemctl disable --now ananicy-cpp.service
sudo systemctl enable --now arbiter.service
```

---

## 配置

arbiter 会按顺序查找配置文件，找到即停止：

1. `/etc/arbiter/config.toml`
2. `$XDG_CONFIG_HOME/arbiter/config.toml`（通常为 `~/.config/arbiter/config.toml`）

若两者都不存在，则使用内置默认值。

### 最小配置示例

```toml
# /etc/arbiter/config.toml

# 规则目录（可指定多个，按序加载）
rules_dirs = [
    "/etc/arbiter/rules.d",
    # 追加自定义规则目录
    # 由于 first-match-wins，较早命中的选择器可能遮蔽后面的规则
    # "/home/user/.config/arbiter/rules.d",
]

# 日志级别：trace / debug / info / warn / error
log_level = "info"

# 工作模式：default / gaming / lowpower / server
profile = "default"

# 试运行模式：只记录日志，不实际写入 /proc 或 cgroup
dry_run = false

# 各类调整开关（默认均为 true）
apply_nice   = true
apply_ionice = true
apply_oom    = true
apply_cgroup = true

# exec 事件到读取 /proc/<pid> 的延迟（毫秒）
# 给进程完成 execve 并填充 /proc 条目留出时间
exec_delay_ms = 50
```

### 使用 CachyOS 规则的推荐配置

将 CachyOS 规则与自定义扩展规则分层放置：

```toml
rules_dirs = [
    "/etc/arbiter/rules.d",       # 先加载 CachyOS 官方规则
    "/etc/arbiter/rules.local.d", # 再加载本地规则
]
```

如果你希望本地规则最终生效，不能只依赖目录顺序，还应让选择器更具体，或者移除前面会先命中的冲突规则。

---

## 静态校验

**在启动守护进程前**，先验证规则文件语法和类型引用均无误：

```sh
# 校验默认 rules_dirs（读取 config.toml）
arbiter check

# 校验指定目录
arbiter check /etc/arbiter/rules.d

# 校验项目内置规则
arbiter check ./rules
```

输出示例：

```sh
OK — 7 types, 42 rules loaded and resolved without errors
```

若有错误，命令以非零退出码退出并列出所有失败项，便于 CI 集成。

---

## 运行守护进程

### 前台试运行（推荐首次部署使用）

```sh
# dry-run：只打印日志，不实际调整进程属性
sudo arbiter daemon --dry-run

# 观察类似以下的日志行即代表匹配正常工作：
# INFO arbiter::daemon: [dry-run] would apply pid=1234 comm="firefox" rule="firefox"
```

### 前台正式运行

```sh
sudo arbiter daemon
```

### 查看当前状态

```sh
arbiter status
# 输出示例：
# Scheduler : scx_bpfland
# Strategy  : NiceAndWeight
# Profile   : default
# Types     : 20
# Rules     : 1847
# Dry-run   : false
```

---

## systemd 服务

创建 systemd 单元文件：

```sh
sudo tee /etc/systemd/system/arbiter.service > /dev/null << 'EOF'
[Unit]
Description=arbiter — scx-aware process priority manager
Documentation=https://github.com/yourrepo/arbiter
After=local-fs.target systemd-udevd.service

[Service]
Type=simple
ExecStart=/usr/local/bin/arbiter daemon
Restart=on-failure
RestartSec=5

# 权限：只需两个 capability，无需完整 root
# 若需要创建 cgroup 目录，则保留 CAP_DAC_OVERRIDE 或以 root 运行
AmbientCapabilities=CAP_NET_ADMIN CAP_SYS_NICE
CapabilityBoundingSet=CAP_NET_ADMIN CAP_SYS_NICE

# 强化隔离
PrivateTmp=true
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=/sys/fs/cgroup /proc

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now arbiter.service

# 查看运行日志
journalctl -u arbiter.service -f
```

---

## 热重载规则

修改规则文件后，无需重启守护进程：

```sh
# 重新加载所有 *.types 和 *.rules 文件
sudo systemctl kill --signal=SIGHUP arbiter.service

# 或直接发信号给进程
sudo kill -HUP $(pidof arbiter)
```

日志中出现以下内容即代表重载成功：

```sh
INFO arbiter::daemon: SIGHUP received — reloading rules
INFO arbiter::daemon: Rules reloaded count=1847
```

> **重要**：`SIGHUP` 只重载规则，**不**重载 `config.toml`。
> 若修改了 `profile` 或其他 config 字段，需完整重启守护进程：
>
> ```sh
> sudo systemctl restart arbiter.service
> ```

---

## 调试与验证

### 检查特定进程是否命中规则

```sh
# 通过进程名
arbiter explain firefox

# 通过 PID
arbiter explain 12345
```

输出示例：

```sh
✓ Matched rule:   firefox
  nice:           0
  oom_score_adj:  0
  cgroup:         desktop.slice
  cgroup_weight:  200
```

### 验证规则实际已生效

```sh
PID=$(pidof firefox | awk '{print $1}')

# 查看 nice 值
cat /proc/$PID/stat | awk '{print "nice:", $19}'

# 查看 IO 优先级
ionice -p $PID

# 查看 oom_score_adj
cat /proc/$PID/oom_score_adj

# 查看 cgroup
cat /proc/$PID/cgroup
```

### 查看详细日志

```sh
# 设置 debug 级别（临时覆盖，不修改 config.toml）
sudo RUST_LOG=debug arbiter daemon --dry-run

# 或在 config.toml 中设置
# log_level = "debug"
```

### 验证 scx 调度器检测

```sh
arbiter status
# Scheduler 行显示检测到的调度器，例如：
# Scheduler : scx_bpfland
# Strategy  : NiceAndWeight
```

---

## 兼容性说明

### 字段兼容性对照表

以下为 CachyOS `ananicy-rules` 字段与 arbiter 的支持情况：

| 字段                             | CachyOS 用法                                  | arbiter | 说明                                                                                                   |
| -------------------------------- | --------------------------------------------- | ------- | ------------------------------------------------------------------------------------------------------ |
| `name`                           | 进程名                                        | ✅      | 匹配 `comm`（≤15 字符）或 exe basename，大小写不敏感                                                   |
| `type`                           | 类型继承                                      | ✅      | 引用 `.types` 中的预设，rule 字段覆盖 type 默认值                                                      |
| `nice`                           | -20~19                                        | ✅      | `setpriority()` 实现，clamp 到 [-20, 19]                                                               |
| `ioclass`                        | `best-effort` / `idle` / `real-time` / `none` | ✅      | 映射为 cgroup v2 `io.weight`（结合 `ionice` 级别）                                                     |
| `ionice`                         | 0~7                                           | ✅      | IO 优先级级别                                                                                          |
| `oom_score_adj`                  | -1000~1000                                    | ✅      | 写入 `/proc/<pid>/oom_score_adj`                                                                       |
| `cgroup`                         | systemd slice 名                              | ✅      | 写入 `/sys/fs/cgroup/user.slice/user-$UID.slice/user@$UID.service/arbiter.slice/<cgroup>/cgroup.procs` |
| `cgroup_weight`                  | CPU 权重                                      | ✅      | 写入 cgroup v2 `cpu.weight`，clamp 到 [1, 10000]                                                       |
| `sched`                          | 调度策略                                      | ⚠️ 忽略 | 字段可解析但不应用（round-trip 兼容）                                                                  |
| `latency_nice`                   | 延迟 nice                                     | ❌ 忽略 | CachyOS 扩展字段，arbiter 不支持                                                                       |
| _(rule only)_ `exe_pattern`      | —                                             | ✅      | arbiter 扩展：对完整 exe 路径做正则匹配                                                                |
| _(rule only)_ `cmdline_contains` | —                                             | ✅      | arbiter 扩展：对 cmdline 做子串匹配                                                                    |

### `.cgroups` 文件迁移

CachyOS 仓库的 `00-cgroups.cgroups` 格式（含 `CPUQuota` 等字段）不会被 arbiter 加载。  
若需等效策略，需手工在 `.types` 中添加 `cgroup` 字段，并在 systemd 中预建对应 slice：

```json
// 在 .types 文件中追加
{
  "type": "BG_CPUIO",
  "nice": 16,
  "ioclass": "idle",
  "cgroup": "background.slice",
  "cgroup_weight": 50
}
```

### 与 Ananicy-cpp 共存

**不要同时运行 arbiter 和 ananicy-cpp**，两者都会修改同一进程的 nice/ionice 属性，导致冲突。  
替换步骤：

```sh
sudo systemctl disable --now ananicy-cpp.service
sudo systemctl enable --now arbiter.service
```

---

## 卸载

```sh
sudo systemctl disable --now arbiter.service
sudo rm /etc/systemd/system/arbiter.service
sudo systemctl daemon-reload

sudo rm /usr/local/bin/arbiter
sudo rm -rf /etc/arbiter
```
