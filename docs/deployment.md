# Arbiter Deployment Guide

This document describes how to deploy `arbiter` as a system-level process priority management daemon, specifically using the official CachyOS `ananicy-rules` rule set.

---

## Table of Contents

1. [Prerequisites](#prerequisites)
2. [Compilation](#compilation)
3. [Installing Rule Sets](#installing-rule-sets)
4. [Configuration](#configuration)
5. [Static Validation](#static-validation)
6. [Running the Daemon](#running-the-daemon)
7. [systemd Service](#systemd-service)
8. [Hot Reloading Rules](#hot-reloading-rules)
9. [Debugging and Verification](#debugging-and-verification)
10. [Compatibility Notes](#compatibility-notes)
11. [Uninstallation](#uninstallation)

---

## Prerequisites

| Requirement    | Description                                                                                                   |
| -------------- | ------------------------------------------------------------------------------------------------------------- |
| Linux Kernel   | ≥ 6.12 (includes `sched_ext` BPF framework; works even without scx schedulers)                                |
| cgroup v2      | Unified hierarchy (`/sys/fs/cgroup`), enabled by default in modern distributions                              |
| Permissions    | `CAP_NET_ADMIN` (to read `CN_PROC` process events) + `CAP_SYS_NICE` (to adjust nice/ionice), or run as `root` |
| Rust Toolchain | See `rust-toolchain.toml`, recommended to use [uv](https://docs.astral.sh/uv/) or `rustup`                    |

To verify cgroup v2:

```sh
stat -f /sys/fs/cgroup | grep Type
# Output containing cgroup2 indicates v2 is active
```

---

## Compilation

```sh
# In the project root directory
cargo build --release
# Output path: target/release/arbiter
```

Optional: Install to system path:

```sh
sudo install -Dm755 target/release/arbiter /usr/local/bin/arbiter
```

---

## Installing Rule Sets

### Using Official CachyOS ananicy-rules

```sh
# Clone official rules
git clone https://github.com/CachyOS/ananicy-rules /tmp/ananicy-rules

# Create arbiter rules directory
sudo mkdir -p /etc/arbiter/rules.d

# Copy type definitions (must be loaded before *.rules)
sudo cp /tmp/ananicy-rules/00-types.types /etc/arbiter/rules.d/

# Copy process rules (select subdirectories as needed)
sudo cp /tmp/ananicy-rules/00-default/*.rules /etc/arbiter/rules.d/ 2>/dev/null || true
sudo find /tmp/ananicy-rules/00-default -name '*.rules' \
    -exec sudo cp {} /etc/arbiter/rules.d/ \;
```

> **Note**: The `00-cgroups.cgroups` file in the CachyOS repository will NOT be loaded by `arbiter` (`arbiter` only reads `*.types` and `*.rules`). To use cgroup policies from there, migrate them manually — see [Compatibility Notes](#compatibility-notes).

### Using Built-in Rules

The `rules/` directory in the project provides a minimal reference set for testing:

```sh
sudo mkdir -p /etc/arbiter/rules.d
sudo cp rules/*.types rules/*.rules /etc/arbiter/rules.d/
```

### User-level Rules (No root required)

Rules can also be placed in the XDG config directory, which `arbiter` will automatically read:

```sh
mkdir -p ~/.config/arbiter/rules.d
cp rules/*.types rules/*.rules ~/.config/arbiter/rules.d/
```

If both directories are configured, both will be loaded; **rules with the same name are prioritized by filesystem discovery order** (alphabetical order within the same directory).

---

## Configuration

`arbiter` searches for the configuration file in the following order and stops at the first one found:

1. `/etc/arbiter/config.toml`
2. `$XDG_CONFIG_HOME/arbiter/config.toml` (typically `~/.config/arbiter/config.toml`)

If neither exists, built-in default values are used.

### Minimal Configuration Example

```toml
# /etc/arbiter/config.toml

# Rules directories (multiple can be specified, loaded in order)
rules_dirs = [
    "/etc/arbiter/rules.d",
    # Append custom rules directory (will override duplicate names)
    # "/home/user/.config/arbiter/rules.d",
]

# Log level: trace / debug / info / warn / error
log_level = "info"

# Working profile: default / gaming / lowpower / server
profile = "default"

# Dry-run mode: logs actions without writing to /proc or cgroup
dry_run = false

# Adjustment toggles (default to true)
apply_nice   = true
apply_ionice = true
apply_oom    = true
apply_cgroup = true

# Delay from exec event to reading /proc/<pid> (milliseconds)
# Gives the process time to complete execve and populate /proc entries
exec_delay_ms = 50
```

### Recommended Configuration for CachyOS Rules

Layer CachyOS rules with local overrides:

```toml
rules_dirs = [
    "/etc/arbiter/rules.d",          # Official CachyOS rules
    "/etc/arbiter/rules.local.d",    # Local override rules (higher priority)
]
```

---

## Static Validation

**Before starting the daemon**, verify that rule syntax and type references are correct:

```sh
# Validate default rules_dirs (reads config.toml)
arbiter check

# Validate a specific directory
arbiter check /etc/arbiter/rules.d

# Validate built-in rules
arbiter check ./rules
```

Example output:

```sh
OK — 7 types, 42 rules loaded and resolved without errors
```

If errors exist, the command exits with a non-zero code and lists all failures, making it suitable for CI integration.

---

## Running the Daemon

### Foreground Dry-run (Recommended for first deployment)

```sh
# dry-run: prints actions without actually adjusting process attributes
sudo arbiter daemon --dry-run

# Observe logs like the following to verify matching:
# INFO arbiter::daemon: [dry-run] would apply pid=1234 comm="firefox" rule="firefox"
```

### Foreground Active Run

```sh
sudo arbiter daemon
```

### Check Current Status

```sh
arbiter status
# Example output:
# Scheduler : scx_bpfland
# Strategy  : NiceAndWeight
# Profile   : default
# Types     : 20
# Rules     : 1847
# Dry-run   : false
```

---

## systemd Service

Create a systemd unit file:

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

# Permissions: requires only these two capabilities, no full root needed
# If cgroup directory creation is needed, keep CAP_DAC_OVERRIDE or run as root
AmbientCapabilities=CAP_NET_ADMIN CAP_SYS_NICE
CapabilityBoundingSet=CAP_NET_ADMIN CAP_SYS_NICE

# Hardening
PrivateTmp=true
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=/sys/fs/cgroup /proc

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now arbiter.service

# View logs
journalctl -u arbiter.service -f
```

---

## Hot Reloading Rules

You can reload rule files without restarting the daemon:

```sh
# Reload all *.types and *.rules files
sudo systemctl kill --signal=SIGHUP arbiter.service

# Or send the signal directly to the process
sudo kill -HUP $(pidof arbiter)
```

The reload is successful if the following appears in the logs:

```sh
INFO arbiter::daemon: SIGHUP received — reloading rules
INFO arbiter::daemon: Rules reloaded count=1847
```

> **Important**: `SIGHUP` only reloads rules, NOT `config.toml`. If you modify `profile` or other config fields, a full restart is required:
>
> ```sh
> sudo systemctl restart arbiter.service
> ```

---

## Debugging and Verification

### Check if a process matches a rule

```sh
# By process name
arbiter explain firefox

# By PID
arbiter explain 12345
```

Example output:

```sh
✓ Matched rule:   firefox
  nice:           0
  oom_score_adj:  0
  cgroup:         desktop.slice
  cgroup_weight:  200
```

### Verify applied attributes

```sh
PID=$(pidof firefox | awk '{print $1}')

# Check nice value
cat /proc/$PID/stat | awk '{print "nice:", $19}'

# Check IO priority
ionice -p $PID

# Check oom_score_adj
cat /proc/$PID/oom_score_adj

# Check cgroup
cat /proc/$PID/cgroup
```

### View Detailed Logs

```sh
# Set debug level (temporary override, doesn't modify config.toml)
sudo RUST_LOG=debug arbiter daemon --dry-run

# Or set in config.toml
# log_level = "debug"
```

### Verify scx scheduler detection

```sh
arbiter status
# The Scheduler line shows the detected scheduler, e.g.:
# Scheduler : scx_bpfland
# Strategy  : NiceAndWeight
```

---

## Compatibility Notes

### Field Compatibility Table

Support status for CachyOS `ananicy-rules` fields in `arbiter`:

| Field                            | CachyOS Usage                                 | arbiter    | Description                                                        |
| -------------------------------- | --------------------------------------------- | ---------- | ------------------------------------------------------------------ |
| `name`                           | Process name                                  | ✅         | Matches `comm` (≤15 chars) or exe basename, case-insensitive       |
| `type`                           | Type inheritance                              | ✅         | References presets in `.types`; rule fields override type defaults |
| `nice`                           | -20 to 19                                     | ✅         | Implemented via `setpriority()`, clamped to [-20, 19]              |
| `ioclass`                        | `best-effort` / `idle` / `real-time` / `none` | ✅         | Implemented via `ioprio_set()`                                     |
| `ionice`                         | 0 to 7                                        | ✅         | IO priority level                                                  |
| `oom_score_adj`                  | -1000 to 1000                                 | ✅         | Writes to `/proc/<pid>/oom_score_adj`                              |
| `cgroup`                         | systemd slice name                            | ✅         | Writes to `/sys/fs/cgroup/<cgroup>/cgroup.procs`                   |
| `cgroup_weight`                  | CPU weight                                    | ✅         | Writes to cgroup v2 `cpu.weight`, clamped to [1, 10000]            |
| `sched`                          | Scheduling policy                             | ⚠️ Ignored | Parsed but not applied (round-trip compatibility)                  |
| `latency_nice`                   | Latency nice                                  | ❌ Ignored | CachyOS extension field, unsupported                               |
| _(rule only)_ `exe_pattern`      | —                                             | ✅         | arbiter extension: regex match on full exe path                    |
| _(rule only)_ `cmdline_contains` | —                                             | ✅         | arbiter extension: substring match on cmdline                      |

### `.cgroups` File Migration

The `00-cgroups.cgroups` format (containing `CPUQuota` etc.) is NOT loaded by `arbiter`. To achieve equivalent policies, add the `cgroup` field to your `.types` and ensure the corresponding slice is pre-created via systemd:

```json
// Append to your .types file
{
  "type": "BG_CPUIO",
  "nice": 16,
  "ioclass": "idle",
  "cgroup": "background.slice",
  "cgroup_weight": 50
}
```

### Coexistence with Ananicy-cpp

**Do NOT run `arbiter` and `ananicy-cpp` simultaneously.** Both will attempt to modify the same process attributes, leading to conflicts. To replace:

```sh
sudo systemctl disable --now ananicy-cpp.service
sudo systemctl enable --now arbiter.service
```

---

## Uninstallation

```sh
sudo systemctl disable --now arbiter.service
sudo rm /etc/systemd/system/arbiter.service
sudo systemctl daemon-reload

sudo rm /usr/local/bin/arbiter
sudo rm -rf /etc/arbiter
```
