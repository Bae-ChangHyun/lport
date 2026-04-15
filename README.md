# lport

> List listening ports on Linux — and **which folder each server was launched from**.

```
PROTO  PORT  PID      PROCESS  JOB                                     CPU   MEM   UPTIME
-----  ----  -------  -------  --------------------------------------  ----  ----  ------
tcp    3000  3478594  python3  /home/bch/Project/scriptable            0.0%  20M   7d19h
tcp    5174  1644291  node     /home/bch/Project/doc-portfolio/editor  0.0%  93M   14d19h
tcp    8080  3571557  python3  /home/bch/obsidian_new/wiki-web         0.0%  62M   22h19m
tcp    1200  -        docker   rsshub-rsshub-1                         -     -     7 weeks
tcp    2222  -        docker   unsloth                                 -     -     4 weeks
```

## About

A tiny (~550 KB, zero-dependency) Rust CLI that answers two questions you actually ask every day:

1. *Which port is `8080`?*
2. *Which folder did I `npm run dev` from to start that thing?*

`lport` shows the **working directory** of each listening server's process — so you instantly know which project a port belongs to. Docker compose containers display their compose project directory.

## Quick start

```bash
curl -sfL https://raw.githubusercontent.com/Bae-ChangHyun/lport/main/install.sh | sh
```

Requires the Rust toolchain (the script tells you how to install it in one line if missing).

Or directly:

```bash
cargo install --git https://github.com/Bae-ChangHyun/lport
```

## Usage

```bash
lport                    # dashboard: user servers + docker containers
lport --dev              # everything (system daemons included)
lport info 8080          # detail block for a single port
lport info 8080 5432     # multiple ports
sudo lport               # full visibility into other users' processes
```

### Detail view

```
$ lport info 8080 2222
─────────────────────────────────────────────
  PORT     tcp/8080
  PROCESS  python3
  PID      3571557
  USER     bch
  CPU      0.0%
  MEM      62M
  THREADS  7
  UPTIME   22h19m
  CWD      /home/bch/obsidian_new/wiki-web
  CMD      /home/bch/obsidian_new/wiki-web/.venv/bin/python3 app.py

─────────────────────────────────────────────
  PORT       tcp/2222 → 22 (in container)
  TYPE       docker container
  CONTAINER  unsloth
  IMAGE      unsloth:latest
  WORKDIR    /home/bch/Project/main_project/unsloth
  CPU        0.11%
  MEM        1.045GiB
  UPTIME     4 weeks
```

## How it works

- `ss -tlnpH` / `ss -ulnpH` for listening sockets
- `/proc/<pid>/cwd` and `/proc/<pid>/cmdline` for process details
- `ps -o pid=,pcpu=,rss=,nlwp=,etime=,user=` (one batched call) for stats
- `docker ps` for container/image/compose-project mapping
- `docker stats --no-stream <name>` (only in `info` mode) for container CPU / MEM

Dashboard runs in ~130 ms. The `info` subcommand adds ~1 s only when a Docker container is involved.

## Requirements

- Linux (uses `/proc` and `ss`)
- `iproute2` (`ss`) and `procps` (`ps`) — present on virtually every distro
- Optional: `docker` for container mapping

## Limitations

- **Linux only.** macOS / BSD not supported.
- Without `sudo`, processes owned by other users show as `?`.
- Containers started with plain `docker run` (not compose) display `WORKDIR: -` — Docker doesn't record the CLI invocation directory.

## License

MIT — see [LICENSE](LICENSE).
