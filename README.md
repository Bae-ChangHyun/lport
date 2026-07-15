<div align="center">

![lport](docs/banner.png)

**List listening ports on Linux and macOS — and the folder each server was launched from.**

[![Release](https://img.shields.io/github/v/release/Bae-ChangHyun/lport?style=flat-square&color=F5B240)](https://github.com/Bae-ChangHyun/lport/releases)
[![License](https://img.shields.io/badge/License-MIT-yellow.svg?style=flat-square)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-Linux%20%7C%20macOS-blue?style=flat-square)](#requirements)
[![Built with](https://img.shields.io/badge/Built%20with-Rust-CE422B?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Zero deps](https://img.shields.io/badge/Dependencies-stdlib%20only-444?style=flat-square)](Cargo.toml)

[Quick start](#quick-start) · [Usage](#usage) · [How it works](#how-it-works) · [Releases](https://github.com/Bae-ChangHyun/lport/releases)

**English** · [한국어](README.ko.md)

</div>

---

## About

A tiny (~550 KB, zero-dependency) Rust CLI that answers two questions you actually ask every day:

1. *Which port is `8080`?*
2. *Which folder did I `npm run dev` from to start that thing?*

`lport` shows the **working directory** of each listening server's process — so you instantly know which project a port belongs to. Docker compose containers are grouped by project, so sibling services (`supabase_db_*`, `supabase_kong_*`, …) read as one block instead of a scattered list. And when you want a port back, `lport kill` doesn't just fire a signal and hope — it **verifies the process actually exited**, offers SIGKILL escalation when it didn't, and warns you when a supervisor immediately restarts it.

### Why?

`lsof -i` and `ss -tlnp` tell you which PID owns a port. They don't tell you which project that PID came from — and they treat every Docker container as just another opaque process.

`lport` adds the missing layer: each row carries the cwd (or the compose project working directory), and Docker rows roll up under a `[ project ]` header so a stack reads as one block.

## Quick start

```bash
curl -sfL https://raw.githubusercontent.com/Bae-ChangHyun/lport/main/install.sh | sh
```

Requires the Rust toolchain (the script tells you how to install it in one line if missing). Re-running the installer detects the version on disk and skips work when you're already on the latest release; pass `--force` to reinstall anyway.

```bash
# fresh install or auto-upgrade
curl -sfL https://raw.githubusercontent.com/Bae-ChangHyun/lport/main/install.sh | sh

# force reinstall
curl -sfL https://raw.githubusercontent.com/Bae-ChangHyun/lport/main/install.sh | sh -s -- --force

# or directly via cargo
cargo install --git https://github.com/Bae-ChangHyun/lport
```

## Usage

```bash
lport                    # dashboard: user servers + docker containers
lport --dev              # everything (system daemons included)
lport info 8080          # detail block for a single port
lport info 8080 5432     # multiple ports
lport kill 3000          # SIGTERM the process(es) listening on port 3000
lport kill -9 3000 8080  # SIGKILL multiple ports
lport kill -y 3000       # skip the [y/N] prompt (non-interactive)
sudo lport               # full visibility into other users' processes
```

### Dashboard

<p align="center">
  <img src="demo/dashboard.gif" alt="lport dashboard" width="780"/>
</p>

The default view groups Docker containers by their `com.docker.compose.project` label (falling back to the container name), so sibling containers of the same compose project read as one `[ project ]` block. Local rows show the full working directory in the `JOB` column with `$HOME` abbreviated to `~`. The column is **never truncated** — losing the path would defeat the feature — and column widths account for CJK and emoji display width, so a path like `~/프로젝트/데모` keeps the table aligned.

### Detail view

<p align="center">
  <img src="demo/info.gif" alt="lport info" width="780"/>
</p>

`lport info PORT...` filters to the requested ports before reading per-PID state, so a single-port query stays cheap. The block surfaces user, CPU, MEM, threads (Linux), uptime, working directory, and the full command line. For Docker-backed ports, it adds container name, image, compose working directory, and live `docker stats` CPU / MEM. Each requested port with no listener is reported individually (`port N: no listening process found.`).

### Killing a port

<p align="center">
  <img src="demo/kill.gif" alt="lport kill" width="780"/>
</p>

`lport kill PORT [PORT ...]` sends `SIGTERM` by default; pass `-9` or `--force` for `SIGKILL`. Each process is confirmed with a `[y/N]` prompt showing its PID, name, and working directory; pass `-y` / `--yes` to skip it for non-interactive callers (scripts, service managers). A PID listening on multiple ports (or on tcp+udp) receives one signal, not many.

Sending a signal is not the same as the process obeying it, so `lport` waits up to 3 seconds for the PID to actually disappear before reporting `killed`:

- **Survived SIGTERM?** An interactive run offers to escalate: `Escalate to SIGKILL? [y/N]`. A non-interactive one prints a `lport kill -9 PORT` hint and exits `1`.
- **Restarted immediately?** When a killed port starts listening again moments later, `lport` warns that a supervisor (dev server, systemd) most likely restarted it — kill the parent instead.
- **No permission?** Failures surface the `kill` diagnostic and hint `sudo lport kill PORT` on EPERM.

Docker-backed ports are **not** killed — `lport` prints the matching `docker stop <name>` command instead. Container lifecycle is outside `lport`'s scope.

```
$ lport kill 5432
port tcp/5432: owned by Docker container 'supabase_db_supabase-prod'. Use: docker stop supabase_db_supabase-prod
```

### Exit codes

| Command | `0` | `1` | `2` |
| --- | --- | --- | --- |
| `lport` / `--dev` | always (even on empty output) | — | unknown argument |
| `lport info` | every requested port found | any requested port had no listener | argument error |
| `lport kill` | every target confirmed dead (or skipped by you) | no listener / Docker-backed / signal failed / survived | argument error |

Exit codes survive closed pipes: `lport kill -y 3000 8080 | head -1` still signals both ports and reports honestly.

## How it works

On **Linux**:

- `ss -tlnpH` and `ss -ulnpH` for TCP/UDP listening sockets (a failing `ss` is reported loudly, never as "no ports")
- `/proc/<pid>/{cwd,cmdline,stat,exe}` read directly — no extra process spawn
- `ps -o pid=,pcpu=,rss=,nlwp=,etime=,user=` (one batched call) for CPU / MEM / uptime / user
- kill verification polls `/proc/<pid>/stat` — zombies count as exited, their sockets are already released

On **macOS**:

- `lsof -nP -iTCP -sTCP:LISTEN` and `lsof -nP -iUDP` for TCP/UDP listening sockets
- `lsof -a -p <pids> -d cwd` (one batched call) for each process's working directory
- `ps -o pid=,tty=,comm=` (pass 1) for TTY + executable basename
- `ps -o pid=,command=` (pass 2) for the full command line
- `ps -o pid=,pcpu=,rss=,etime=,user=` for CPU / MEM / uptime / user
- kill verification polls `ps -o stat=` — not `kill -0`, which mistakes zombies for live processes

BSD `ps` on macOS has no `nlwp` (thread count), so the `THREADS` row is Linux-only.

And on both:

- `docker ps` for container/image/compose-project mapping, keyed by `(proto, host-port)` and matched against the listener's **bind address and IP stack** — a container published on one host IP (or one stack) never claims an unrelated listener on the same port number, and a dual-stack publish (`0.0.0.0` + `[::]`) collapses into one row. The `com.docker.compose.project` label drives the dashboard grouping; rows fall back to the container name when there is no label.
- `docker stats --no-stream <name>` (only in `info` mode) for container CPU / MEM, retried once if a container disappeared in between

Dashboard runs in ~130 ms on Linux. macOS is slightly slower because it shells out to `lsof` / `ps` instead of reading `/proc`. The `info` and `kill` subcommands filter to the requested port(s) before enriching, so single-port operations do not pay the whole-system cost; Docker adds ~1 s when a container is involved.

## Requirements

- Linux or macOS
- `ps` — present on every Unix
- Linux: `iproute2` (`ss`) — present on virtually every distro
- macOS: `lsof` — preinstalled
- Optional: `docker` for container mapping
- Optional: `curl` for the background update check (skipped if missing)

## Limitations

- **Unix only.** Windows is not supported.
- Without `sudo`, processes owned by other users show as `?` (Linux) or are hidden entirely (macOS — `lsof` cannot read foreign process state without privileges). Listeners with unknown PIDs are shown one row per socket — `lport` won't guess that two invisible sockets belong to the same process.
- Containers started with plain `docker run` (not compose) display `WORKDIR: -` — Docker doesn't record the CLI invocation directory.
- With `userland-proxy: false` in `dockerd`, a published port has no host listener of its own (traffic is routed by iptables). An unrelated local process bound to that same port is then shown as the container's row.

## License

MIT — see [LICENSE](LICENSE).
