# Arbiter 部署指南

本文档说明如何在配合 CachyOS `ananicy-rules` 官方规则集的情况下，将 arbiter 部署为系统级进程优先级管理守护进程。

---

## 目录

1. [前置要求](#前置要求)
2. [编译](#编译)
3. [安装规则集](#安装规则集)
4. [配置](#配置)
5. [静态校验](#静态校验)
6. [运行守护进程](#运行守护进程)
7. [systemd 服务](#systemd-服务)
8. [热重载规则](#热重载规则)
9. [调试与验证](#调试与验证)
10. [兼容性说明](#兼容性说明)
11. [卸载](#卸载)

---

## 前置要求

| 要求        | 说明                                                                                                 |
| ----------- | ---------------------------------------------------------------------------------------------------- |
| Linux 内核  | ≥ 6.12（含 `sched_ext` BPF 框架；无 scx 调度器时也可正常工作）                                       |
| cgroup v2   | 统一层级（`/sys/fs/cgroup`），现代发行版默认已启用                                                   |
| 权限        | `CAP_NET_ADMIN`（读取 `CN_PROC` 进程事件）+ `CAP_SYS_NICE`（调整 nice/ionice），或直接以 `root` 运行 |
| Rust 工具链 | 见 `rust-toolchain.toml`，建议使用 [uv](https://docs.astral.sh/uv/) 或 `rustup` 管理                 |

验证 cgroup v2：

```sh
stat -f /sys/fs/cgroup | grep Type
# 输出含 cgroup2 即为 v2
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

两个目录都配置时均会加载；**同名规则以文件系统中先被 glob 到的为准**（同目录内按文件名字母序）。

---

## 配置

arbiter 会按序查找配置文件，找到即停止：

1. `/etc/arbiter/config.toml`
2. `$XDG_CONFIG_HOME/arbiter/config.toml`（通常为 `~/.config/arbiter/config.toml`）

若均不存在，使用内置默认值。

### 最小配置示例

```toml
# /etc/arbiter/config.toml

# 规则目录（可指定多个，按序加载）
rules_dirs = [
    "/etc/arbiter/rules.d",
    # 追加自定义规则目录（会覆盖同名规则）
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
    "/etc/arbiter/rules.d",          # CachyOS 官方规则
    "/etc/arbiter/rules.local.d",    # 本地覆盖规则（优先级更高）
]
```

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

| 字段                             | CachyOS 用法                                  | arbiter | 说明                                                 |
| -------------------------------- | --------------------------------------------- | ------- | ---------------------------------------------------- |
| `name`                           | 进程名                                        | ✅      | 匹配 `comm`（≤15 字符）或 exe basename，大小写不敏感 |
| `type`                           | 类型继承                                      | ✅      | 引用 `.types` 中的预设，rule 字段覆盖 type 默认值    |
| `nice`                           | -20~19                                        | ✅      | `setpriority()` 实现，clamp 到 [-20, 19]             |
| `ioclass`                        | `best-effort` / `idle` / `real-time` / `none` | ✅      | `ioprio_set()` 实现                                  |
| `ionice`                         | 0~7                                           | ✅      | IO 优先级级别                                        |
| `oom_score_adj`                  | -1000~1000                                    | ✅      | 写入 `/proc/<pid>/oom_score_adj`                     |
| `cgroup`                         | systemd slice 名                              | ✅      | 写入 `/sys/fs/cgroup/<cgroup>/cgroup.procs`          |
| `cgroup_weight`                  | CPU 权重                                      | ✅      | 写入 cgroup v2 `cpu.weight`，clamp 到 [1, 10000]     |
| `sched`                          | 调度策略                                      | ⚠️ 忽略 | 字段可解析但不应用（round-trip 兼容）                |
| `latency_nice`                   | 延迟 nice                                     | ❌ 忽略 | CachyOS 扩展字段，arbiter 不支持                     |
| _(rule only)_ `exe_pattern`      | —                                             | ✅      | arbiter 扩展：对完整 exe 路径做正则匹配              |
| _(rule only)_ `cmdline_contains` | —                                             | ✅      | arbiter 扩展：对 cmdline 做子串匹配                  |

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
