use std::collections::{HashMap, HashSet};
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(target_os = "linux")]
use std::fs;

macro_rules! outln {
    ($out:expr, $($arg:tt)*) => {{
        if let Err(e) = writeln!($out, $($arg)*) {
            if e.kind() == io::ErrorKind::BrokenPipe {
                std::process::exit(0);
            }
        }
    }};
}

// Reporting variant for the kill path. A closed pipe (`lport kill 3000 8080 | head -1`)
// must not abort the run: the remaining ports still have to be signaled, and the exit
// code still has to reflect what happened. Kill output is best-effort; the signals are not.
macro_rules! outln_soft {
    ($out:expr, $($arg:tt)*) => {{
        let _ = writeln!($out, $($arg)*);
    }};
}

#[derive(Default, Clone)]
struct Stats {
    cpu: String,
    mem: String,
    uptime: String,
    threads: Option<u32>,
    user: Option<String>,
}

#[derive(Clone)]
struct DockerInfo {
    name: String,
    image: String,
    running_for: String,
    work_dir: Option<String>,
    container_port: u32,
    // Host address the port is published on ("0.0.0.0", "::", "192.168.1.10").
    // A container published on one address must not claim a listener bound to a
    // different one on the same port.
    host_ip: String,
    // `com.docker.compose.project` label. Used to group sibling containers of
    // the same compose project (e.g. all `supabase_*_helchang` rows) under one
    // visual block.
    compose_project: Option<String>,
}

struct Entry {
    proto: &'static str,
    port: u32,
    pid: Option<u32>,
    process: String,
    // Bind address for display, scope suffix kept ("127.0.0.1", "::", "*",
    // "0.0.0.0%virbr0"). Distinct from the match key in `split_bind_addr`, which
    // strips the scope.
    addr: String,
    cwd: String,
    cmdline: String,
    // Parent (ppid, cmdline). Filled only in info mode — see enrich_parent_info —
    // so the dashboard and kill paths pay nothing for it.
    parent: Option<(u32, String)>,
    docker: Option<DockerInfo>,
    stats: Stats,
    user_launched: bool,
}

enum Mode {
    Dashboard { dev: bool },
    Info { ports: Vec<u32> },
    Kill { ports: Vec<u32>, force: bool, yes: bool },
}

fn main() {
    // args_os + lossy: `std::env::args()` panics on non-UTF8 arguments, which is
    // a crash where an "unknown argument" error is the right answer.
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return;
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("lport {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let mode = parse_mode(&args);
    let docker_map = load_docker_ports();

    let mut entries = collect_listening(&docker_map, true);

    entries.sort_by(|a, b| {
        (a.port, a.proto, a.pid.unwrap_or(0)).cmp(&(b.port, b.proto, b.pid.unwrap_or(0)))
    });
    // Only dedup rows whose PID is known. `None == None` is not "the same process",
    // it is "two processes lport cannot see" (root-owned listeners without sudo) —
    // each row is a distinct socket, and merging them hides a listener.
    //
    // Docker rows are the exception: a container published on both stacks (docker's
    // default: `0.0.0.0:P` + `[::]:P`) yields two listeners carrying identical
    // container information and a docker-proxy PID that only differs under sudo, so
    // merging by container identity hides nothing, and docker rows are never kill
    // targets.
    //
    // A merged row must still carry every address it now stands for: `127.0.0.1`-only
    // and `[::]` (all interfaces) have very different exposure, and collapsing them to
    // the first address alone would misreport a public listener as loopback-only. So
    // `dedup_by` accumulates the removed row's addr onto the kept row (`b`).
    entries.dedup_by(|a, b| {
        let same = a.port == b.port
            && a.proto == b.proto
            && match (&a.docker, &b.docker) {
                (Some(da), Some(db)) => da.name == db.name && da.container_port == db.container_port,
                _ => a.pid.is_some() && a.pid == b.pid,
            };
        if same && !a.addr.is_empty() && !b.addr.split(", ").any(|s| s == a.addr) {
            if !b.addr.is_empty() {
                b.addr.push_str(", ");
            }
            b.addr.push_str(&a.addr);
        }
        same
    });

    // Info / Kill modes are single-port queries, so filter before the per-PID
    // enrich step. Otherwise `lport info 8080` pays the cost of reading /proc
    // (or calling ps/lsof) for every listener on the box.
    match &mode {
        Mode::Info { ports } | Mode::Kill { ports, .. } => {
            entries.retain(|e| ports.contains(&e.port));
        }
        _ => {}
    }

    enrich_process_info(&mut entries);

    if let Mode::Kill { ports, force, yes } = &mode {
        let code = run_kill(&entries, ports, *force, *yes, &docker_map);
        maybe_print_update_notice();
        std::process::exit(code);
    }

    if let Mode::Dashboard { dev: false } = &mode {
        entries.retain(|e| e.docker.is_some() || e.user_launched);
    }

    // Sort by visual group key (compose project if present, else container
    // name for docker, else cwd for local), then container name for
    // determinism within a group, then port/proto.
    entries.sort_by(|a, b| {
        let group_a = group_key(a);
        let group_b = group_key(b);
        let name_a = a.docker.as_ref().map(|d| d.name.as_str()).unwrap_or("");
        let name_b = b.docker.as_ref().map(|d| d.name.as_str()).unwrap_or("");
        (group_a, name_a, a.port, a.proto).cmp(&(group_b, name_b, b.port, b.proto))
    });

    enrich_local_stats(&mut entries);
    let with_docker_cpu_mem = matches!(mode, Mode::Info { .. });
    enrich_docker_stats(&mut entries, with_docker_cpu_mem);

    match mode {
        Mode::Info { ports } => {
            enrich_parent_info(&mut entries);
            let code = print_info(&entries, &ports);
            maybe_print_update_notice();
            std::process::exit(code);
        }
        Mode::Dashboard { dev } => print_table(&entries, dev),
        // Kill mode already exited via `run_kill` above; this arm exists only
        // to satisfy exhaustiveness.
        Mode::Kill { .. } => unreachable!(),
    }

    maybe_print_update_notice();
}

fn print_help() {
    println!("Usage: lport [--dev]");
    println!("       lport PORT [PORT ...]");
    println!("       lport info PORT [PORT ...]");
    println!("       lport kill PORT [PORT ...] [-9|--force] [-y|--yes]");
    println!();
    println!("  (default)        Show user-launched servers and Docker containers only");
    println!("                   (PROTO PORT PID PROCESS JOB CPU MEM UPTIME)");
    println!("  --dev            Show every listening port, including system daemons");
    println!("  PORT...          Shorthand for `lport info PORT...`");
    println!("  info PORT...     Show full details for the given port(s),");
    println!("                   including Docker container CPU/MEM");
    println!("                   example: lport info 8080 5432");
    println!("  kill PORT...     Terminate the process(es) listening on the given port(s).");
    println!("                   Sends SIGTERM by default; pass -9 / --force for SIGKILL.");
    println!("                   Prompts [y/N] for each process before signaling.");
    println!("                   Pass -y / --yes to skip the prompt (non-interactive).");
    println!("                   Verifies the process actually exited; offers SIGKILL");
    println!("                   escalation if it survives SIGTERM.");
    println!("                   Docker-backed ports are not killed; lport offers");
    println!("                   `docker stop` interactively (or prints the command");
    println!("                   when non-interactive).");
    println!("                   example: lport kill 3000 8080");
    println!("  -V, --version    Print version and exit");
    println!("  -h, --help       Print this help and exit");
    println!();
    println!("Permissions:");
    println!("  Processes owned by other users require elevated privileges to inspect");
    println!("  or kill:");
    println!("    Linux:  PID/PROCESS appear as '?' without sudo.");
    println!("    macOS:  other users' listeners are hidden entirely without sudo.");
    println!("  Run with `sudo lport` for full visibility across users.");
}

enum Confirm {
    Yes,
    No,
    NoTty,
}

/// Prompt the user to confirm killing a single process. Returns `Yes` only on an
/// explicit "y"/"yes". A non-interactive stdin (pipe, no TTY) cannot answer, so
/// it returns `NoTty` and refuses rather than killing silently — same instinct
/// as `rm -i`.
///
/// The prompt goes to stderr: stdout carries the result lines and may be piped,
/// which would otherwise leave the caller staring at a silent read.
fn confirm_kill(entry: &Entry, port: u32, signal_name: &str) -> Confirm {
    if !io::stdin().is_terminal() {
        return Confirm::NoTty;
    }
    let pid = entry.pid.unwrap_or(0);
    let cwd = if entry.cwd.is_empty() { "-" } else { &entry.cwd };
    match prompt_yes_no(&format!(
        "Kill pid {} ({}) on {}/{} [cwd: {}] with {}? [y/N] ",
        pid, entry.process, entry.proto, port, cwd, signal_name
    )) {
        Some(true) => Confirm::Yes,
        Some(false) => Confirm::No,
        // A stderr that cannot be written to is a stderr the user cannot read: consent
        // is unobtainable, exactly as with no TTY. Refuse through the same path.
        None => Confirm::NoTty,
    }
}

/// `None` means the question could not be asked. Never exits: a dead prompt must not
/// cut the kill run short with a success code — the caller decides, and its answer is
/// always to refuse.
fn prompt_yes_no(prompt: &str) -> Option<bool> {
    let mut err = io::stderr();
    if write!(err, "{}", prompt).is_err() {
        return None;
    }
    let _ = err.flush();
    let mut answer = String::new();
    Some(
        io::stdin().read_line(&mut answer).is_ok()
            && matches!(answer.trim(), "y" | "Y" | "yes" | "Yes" | "YES"),
    )
}

// A signal is only *delivered*, never *obeyed*: a process can ignore SIGTERM, and
// even SIGKILL cannot reap one stuck in uninterruptible I/O. So `lport kill` waits
// for the PID to actually disappear before it claims the port is free.
const KILL_WAIT_MS: u64 = 3000;
const ESCALATE_WAIT_MS: u64 = 2000;

#[cfg(target_os = "linux")]
fn pid_alive(pid: u32) -> bool {
    let Ok(stat) = fs::read_to_string(format!("/proc/{}/stat", pid)) else {
        return false;
    };
    let Some(rparen) = stat.rfind(')') else {
        return false;
    };
    let state = stat[rparen + 1..].split_whitespace().next().unwrap_or("");
    // A zombie (or dying) process has already released its sockets — the port is
    // free even though the /proc entry lingers until the parent reaps it.
    !matches!(state, "Z" | "X")
}

#[cfg(target_os = "macos")]
fn pid_alive(pid: u32) -> bool {
    // Not `kill -0`: that succeeds for a zombie, and a zombie has already released
    // its sockets — the port is free even though the entry lingers until the parent
    // reaps it. BSD `ps` reports zombies with a state starting in 'Z'.
    let Ok(output) = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let state = String::from_utf8_lossy(&output.stdout);
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}

/// Poll until the PID is gone. Returns true if the exit was observed.
fn wait_for_exit(pid: u32, timeout_ms: u64) -> bool {
    let mut waited = 0;
    while waited < timeout_ms {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
        waited += 100;
    }
    !pid_alive(pid)
}

// `kill` writes its own diagnostic ("Operation not permitted", "No such process")
// to stderr. It is captured rather than inherited so it cannot interleave with
// lport's output, then replayed under the failing port.
fn report_kill_failure(
    err: &mut impl Write,
    proto: &str,
    port: u32,
    pid: u32,
    signal_flag: &str,
    status: std::process::ExitStatus,
    stderr_bytes: &[u8],
) {
    outln_soft!(
        err,
        "port {}/{}: `kill {} {}` exited with status {}.",
        proto,
        port,
        signal_flag,
        pid,
        status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "?".into())
    );
    let detail = String::from_utf8_lossy(stderr_bytes);
    let detail = detail.trim();
    if !detail.is_empty() {
        outln_soft!(err, "  {}", detail);
        if detail.to_ascii_lowercase().contains("not permitted") {
            outln_soft!(err, "hint: try `sudo lport kill {}`", port);
        }
    }
}

fn run_kill(
    entries: &[Entry],
    ports: &[u32],
    force: bool,
    yes: bool,
    docker_map: &DockerMap,
) -> i32 {
    let signal_flag = if force { "-KILL" } else { "-TERM" };
    let signal_name = if force { "SIGKILL" } else { "SIGTERM" };
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    let mut any_error = false;
    // Dedup across ports: a single PID listening on tcp+udp or on multiple
    // ports given on one command line should only receive one signal. Only
    // PIDs actually signaled (or confirmed already exited) belong here.
    let mut signaled: HashSet<u32> = HashSet::new();
    // PIDs the user declined at the prompt: skip them on later ports without
    // claiming they were handled — no signal was ever sent.
    let mut declined: HashSet<u32> = HashSet::new();
    // PIDs whose first signal could not be delivered (EPERM, `kill` failed to spawn).
    // Same reason as `declined`: nothing reached the process, so "already handled"
    // would be a lie on the next port.
    let mut failed: HashSet<u32> = HashSet::new();
    // Ports whose owner was confirmed dead — the only ones worth re-scanning for
    // a supervisor-spawned replacement.
    let mut freed_ports: HashSet<u32> = HashSet::new();
    // PIDs confirmed dead. A PID holding several of the requested ports is signaled once,
    // so the later ports never reach a `freed_ports.insert` of their own — but they were
    // freed too, and a supervisor can take any of them back.
    let mut dead: HashSet<u32> = HashSet::new();
    // Containers already `docker stop`ped / skipped this run: the same container can back
    // several requested ports (or tcp+udp), and each decision is made once.
    let mut stopped_containers: HashSet<String> = HashSet::new();
    let mut skipped_containers: HashSet<String> = HashSet::new();

    for &port in ports {
        let matches: Vec<&Entry> = entries.iter().filter(|e| e.port == port).collect();
        if matches.is_empty() {
            outln_soft!(err, "port {}: no listening process found.", port);
            any_error = true;
            continue;
        }

        for entry in &matches {
            if let Some(d) = &entry.docker {
                if stopped_containers.contains(&d.name) {
                    outln_soft!(out, "container '{}' already stopped.", d.name);
                    continue;
                }
                if skipped_containers.contains(&d.name) {
                    outln_soft!(out, "skipped container '{}' earlier.", d.name);
                    continue;
                }
                // A container is not killed like a process — stopping it is a container
                // lifecycle action. Offer it interactively; `-y` deliberately does NOT
                // auto-stop containers, since a script clearing a port should not tear
                // down a container as a side effect.
                let interactive = !yes && io::stdin().is_terminal();
                if !interactive {
                    outln_soft!(
                        err,
                        "port {}/{}: owned by Docker container '{}'. Use: docker stop {}",
                        entry.proto,
                        port,
                        d.name,
                        d.name
                    );
                    any_error = true;
                    continue;
                }
                match prompt_yes_no(&format!(
                    "port {}/{} is owned by Docker container '{}'. Stop the container? [y/N] ",
                    entry.proto, port, d.name
                )) {
                    Some(true) => {
                        match Command::new("docker").args(["stop", &d.name]).output() {
                            Ok(o) if o.status.success() => {
                                stopped_containers.insert(d.name.clone());
                                outln_soft!(
                                    out,
                                    "stopped container '{}' ({}/{}).",
                                    d.name,
                                    entry.proto,
                                    port
                                );
                            }
                            Ok(o) => {
                                outln_soft!(
                                    err,
                                    "port {}/{}: `docker stop {}` exited with status {}.",
                                    entry.proto,
                                    port,
                                    d.name,
                                    o.status
                                        .code()
                                        .map(|c| c.to_string())
                                        .unwrap_or_else(|| "?".into())
                                );
                                let detail = String::from_utf8_lossy(&o.stderr);
                                let detail = detail.trim();
                                if !detail.is_empty() {
                                    outln_soft!(err, "  {}", detail);
                                }
                                any_error = true;
                            }
                            Err(e) => {
                                outln_soft!(
                                    err,
                                    "port {}/{}: failed to spawn `docker`: {}.",
                                    entry.proto,
                                    port,
                                    e
                                );
                                any_error = true;
                            }
                        }
                    }
                    Some(false) => {
                        skipped_containers.insert(d.name.clone());
                        outln_soft!(out, "skipped container '{}'. Use: docker stop {}", d.name, d.name);
                    }
                    None => {
                        outln_soft!(
                            err,
                            "port {}/{}: owned by Docker container '{}'. Use: docker stop {}",
                            entry.proto,
                            port,
                            d.name,
                            d.name
                        );
                        any_error = true;
                    }
                }
                continue;
            }
            let Some(pid) = entry.pid else {
                outln_soft!(
                    err,
                    "port {}/{}: PID unknown (insufficient privileges?). Try `sudo lport kill {}`.",
                    entry.proto,
                    port,
                    port
                );
                any_error = true;
                continue;
            };
            if signaled.contains(&pid) {
                // Same PID, another requested port: it was freed by the same signal, so it
                // belongs in the restart re-scan even though nothing was signaled here.
                if dead.contains(&pid) {
                    freed_ports.insert(port);
                }
                outln_soft!(
                    out,
                    "pid {} already handled (also listens on {}/{}).",
                    pid,
                    entry.proto,
                    port
                );
                continue;
            }
            if declined.contains(&pid) {
                outln_soft!(out, "pid {} declined earlier; skipping {}/{}.", pid, entry.proto, port);
                continue;
            }
            if failed.contains(&pid) {
                outln_soft!(
                    out,
                    "pid {} could not be signaled earlier; skipping {}/{}.",
                    pid,
                    entry.proto,
                    port
                );
                continue;
            }
            // Confirm before signaling so a mistyped port cannot take down the
            // wrong process. Show enough identity (pid, name, cwd) to verify.
            // `-y`/`--yes` skips the prompt for non-interactive callers (scripts,
            // service managers) that cannot answer a TTY question.
            let decision = if yes {
                Confirm::Yes
            } else {
                confirm_kill(entry, port, signal_name)
            };
            match decision {
                Confirm::Yes => {}
                Confirm::No => {
                    declined.insert(pid);
                    outln_soft!(out, "skipped pid {} ({}).", pid, entry.process);
                    continue;
                }
                Confirm::NoTty => {
                    declined.insert(pid);
                    outln_soft!(
                        err,
                        "port {}/{}: refusing to kill pid {} ({}) without confirmation (no TTY).",
                        entry.proto,
                        port,
                        pid,
                        entry.process
                    );
                    any_error = true;
                    continue;
                }
            }
            // The prompt above blocks indefinitely; the process may have exited while
            // the user was deciding. Signaling a dead PID fails with "No such process",
            // which would report a failure for a port that is in fact free.
            if !pid_alive(pid) {
                signaled.insert(pid);
                outln_soft!(out, "pid {} ({}) already exited.", pid, entry.process);
                freed_ports.insert(port);
                dead.insert(pid);
                continue;
            }
            signaled.insert(pid);
            let output = Command::new("kill")
                .args([signal_flag, &pid.to_string()])
                .output();
            match output {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    signaled.remove(&pid);
                    failed.insert(pid);
                    report_kill_failure(
                        &mut err,
                        entry.proto,
                        port,
                        pid,
                        signal_flag,
                        o.status,
                        &o.stderr,
                    );
                    any_error = true;
                    continue;
                }
                Err(spawn_err) => {
                    signaled.remove(&pid);
                    failed.insert(pid);
                    outln_soft!(
                        err,
                        "port {}/{}: failed to spawn `kill`: {}.",
                        entry.proto,
                        port,
                        spawn_err
                    );
                    any_error = true;
                    continue;
                }
            }

            if wait_for_exit(pid, KILL_WAIT_MS) {
                outln_soft!(
                    out,
                    "killed pid {} ({}) on {}/{} [{}]",
                    pid,
                    entry.process,
                    entry.proto,
                    port,
                    signal_name
                );
                freed_ports.insert(port);
                dead.insert(pid);
                continue;
            }

            if force {
                outln_soft!(
                    err,
                    "port {}/{}: pid {} ({}) still alive {}s after SIGKILL (uninterruptible I/O?).",
                    entry.proto,
                    port,
                    pid,
                    entry.process,
                    KILL_WAIT_MS / 1000
                );
                any_error = true;
                continue;
            }

            outln_soft!(
                err,
                "port {}/{}: pid {} ({}) still alive {}s after SIGTERM.",
                entry.proto,
                port,
                pid,
                entry.process,
                KILL_WAIT_MS / 1000
            );
            if yes || !io::stdin().is_terminal() {
                outln_soft!(err, "hint: retry with `lport kill -9 {}`", port);
                any_error = true;
                continue;
            }
            if !matches!(prompt_yes_no("Escalate to SIGKILL? [y/N] "), Some(true)) {
                outln_soft!(out, "skipped escalation for pid {} ({}).", pid, entry.process);
                any_error = true;
                continue;
            }
            // A slow-but-obedient process can finish shutting down while the escalation
            // prompt waits — SIGTERM worked, it just took longer than KILL_WAIT_MS.
            if !pid_alive(pid) {
                outln_soft!(
                    out,
                    "pid {} ({}) exited on its own after SIGTERM.",
                    pid,
                    entry.process
                );
                freed_ports.insert(port);
                dead.insert(pid);
                continue;
            }
            let output = Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .output();
            match output {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    report_kill_failure(
                        &mut err,
                        entry.proto,
                        port,
                        pid,
                        "-KILL",
                        o.status,
                        &o.stderr,
                    );
                    any_error = true;
                    continue;
                }
                Err(spawn_err) => {
                    outln_soft!(
                        err,
                        "port {}/{}: failed to spawn `kill`: {}.",
                        entry.proto,
                        port,
                        spawn_err
                    );
                    any_error = true;
                    continue;
                }
            }
            if wait_for_exit(pid, ESCALATE_WAIT_MS) {
                outln_soft!(
                    out,
                    "killed pid {} ({}) on {}/{} [SIGKILL]",
                    pid,
                    entry.process,
                    entry.proto,
                    port
                );
                freed_ports.insert(port);
                dead.insert(pid);
            } else {
                outln_soft!(
                    err,
                    "port {}/{}: pid {} ({}) still alive {}s after SIGKILL (uninterruptible I/O?).",
                    entry.proto,
                    port,
                    pid,
                    entry.process,
                    ESCALATE_WAIT_MS / 1000
                );
                any_error = true;
            }
        }
    }

    warn_on_restart(&mut err, &freed_ports, &signaled, &declined, &failed, docker_map);

    if any_error {
        1
    } else {
        0
    }
}

// A freed port that is listening again seconds later was not freed for the user:
// something (nodemon, a systemd unit, docker restart policy) put a new process
// there. The kill itself succeeded, so this warns without failing the exit code.
fn warn_on_restart(
    err: &mut impl Write,
    freed_ports: &HashSet<u32>,
    signaled: &HashSet<u32>,
    declined: &HashSet<u32>,
    failed: &HashSet<u32>,
    docker_map: &DockerMap,
) {
    if freed_ports.is_empty() {
        return;
    }
    std::thread::sleep(Duration::from_millis(500));
    let mut warned: HashSet<(&'static str, u32, u32)> = HashSet::new();
    for e in collect_listening(docker_map, false) {
        if e.docker.is_some() || !freed_ports.contains(&e.port) {
            continue;
        }
        let Some(pid) = e.pid else { continue };
        // A declined PID — or one whose signal never got delivered — was on the port all
        // along. Neither is a supervisor reviving anything, so neither deserves a restart
        // warning; the port simply still belongs to whoever already had it.
        if signaled.contains(&pid)
            || declined.contains(&pid)
            || failed.contains(&pid)
            || !warned.insert((e.proto, e.port, pid))
        {
            continue;
        }
        outln_soft!(
            err,
            "warning: port {}/{} is listening again (pid {}, {}) — likely restarted by a supervisor (dev server, systemd). Inspect with `lport info {}`.",
            e.proto,
            e.port,
            pid,
            e.process,
            e.port
        );
    }
}

fn parse_mode(args: &[String]) -> Mode {
    // A subcommand is only a subcommand in first position. Matching it anywhere
    // turns `lport kill 8080 info` into "unknown argument 'kill'" instead of the
    // real complaint, which is the stray `info`.
    let subcommand = args.first().map(String::as_str);

    // `lport 8080` is shorthand for `lport info 8080`: if the first argument is a
    // port number, treat every argument as a port. `--dev` / `info` / `kill` still
    // win because they do not parse as a port.
    let shorthand_info = args
        .first()
        .and_then(|a| a.parse::<u32>().ok())
        .is_some_and(|p| (1..=65535).contains(&p));

    if subcommand == Some("info") || shorthand_info {
        let port_args = if shorthand_info { &args[..] } else { &args[1..] };
        let mut ports: Vec<u32> = Vec::new();
        for a in port_args {
            match a.parse::<u32>() {
                Ok(p) if (1..=65535).contains(&p) => ports.push(p),
                _ => {
                    eprintln!("error: invalid port '{}'", a);
                    std::process::exit(2);
                }
            }
        }
        if ports.is_empty() {
            eprintln!("error: 'lport info' requires at least one port number.");
            std::process::exit(2);
        }
        return Mode::Info { ports };
    }

    if subcommand == Some("kill") {
        let mut ports: Vec<u32> = Vec::new();
        let mut force = false;
        let mut yes = false;
        for a in &args[1..] {
            match a.as_str() {
                "-9" | "--force" => force = true,
                "-y" | "--yes" => yes = true,
                _ => match a.parse::<u32>() {
                    Ok(p) if (1..=65535).contains(&p) => ports.push(p),
                    _ => {
                        eprintln!("error: invalid port '{}'", a);
                        std::process::exit(2);
                    }
                },
            }
        }
        if ports.is_empty() {
            eprintln!("error: 'lport kill' requires at least one port number.");
            std::process::exit(2);
        }
        return Mode::Kill { ports, force, yes };
    }

    let mut dev = false;
    for a in args {
        match a.as_str() {
            "--dev" => dev = true,
            _ => {
                eprintln!("error: unknown argument '{}'", a);
                std::process::exit(2);
            }
        }
    }
    Mode::Dashboard { dev }
}

fn is_interpreter_exe(name: &str) -> bool {
    // Strip trailing version digits / dots (e.g. "python3.11" -> "python")
    let stem: String = name
        .chars()
        .take_while(|c| !c.is_ascii_digit() && *c != '.')
        .collect();
    matches!(
        stem.as_str(),
        "python"
            | "node"
            | "deno"
            | "bun"
            | "ruby"
            | "java"
            | "php"
            | "php-fpm"
            | "perl"
            | "dotnet"
            | "erl"
            | "beam"
            | "uvicorn"
            | "gunicorn"
            | "hypercorn"
            | "daphne"
            | "puma"
            | "unicorn"
            | "rails"
    )
}

// One host (proto, port) can carry several publishes — `-p 127.0.0.1:8080:80`
// and `-p 192.168.1.10:8080:80` on different containers coexist — so the value
// is a list and the bind address decides which one owns a given listener.
type DockerMap = HashMap<(&'static str, u32), Vec<DockerInfo>>;

// Bind address of a socket string: "0.0.0.0:8080" -> "0.0.0.0",
// "[::]:8080" -> "::", "*:5353" -> "*".
//
// `ss` appends a scope/interface to addresses bound to one link ("0.0.0.0%virbr0:67",
// "[fe80::1%eth0]:8080"). Docker never reports it, so the suffix is dropped —
// otherwise "0.0.0.0%virbr0" fails the wildcard test and no address ever compares equal.
fn split_bind_addr(local: &str) -> &str {
    match local.rfind(':') {
        Some(i) => {
            let addr = local[..i].trim_start_matches('[').trim_end_matches(']');
            addr.split('%').next().unwrap_or(addr)
        }
        None => "",
    }
}

// Bind address for display: like `split_bind_addr` but keeps the `%scope` suffix,
// since which interface a socket is bound to is information the user wants to see.
fn display_bind_addr(local: &str) -> &str {
    match local.rfind(':') {
        Some(i) => local[..i].trim_start_matches('[').trim_end_matches(']'),
        None => "",
    }
}

fn is_wildcard_addr(a: &str) -> bool {
    matches!(a, "" | "0.0.0.0" | "::" | "*")
}

// A socket bound to a v4-mapped v6 address is reported as "::ffff:127.0.0.1"
// while docker reports the plain "127.0.0.1"; compare them on equal terms.
fn normalize_addr(a: &str) -> &str {
    a.strip_prefix("::ffff:").unwrap_or(a)
}

fn ip_matches(docker_ip: &str, local_addr: &str) -> bool {
    let d = normalize_addr(docker_ip);
    let l = normalize_addr(local_addr);
    // "*" / empty carry no stack information (macOS lsof prints "*:port").
    if d.is_empty() || d == "*" || l.is_empty() || l == "*" {
        return true;
    }
    // A v4 publish must not claim a v6 listener (or vice versa): they are
    // different sockets that merely share a port number.
    if d.contains(':') != l.contains(':') {
        return false;
    }
    d == l || is_wildcard_addr(d) || is_wildcard_addr(l)
}

// Known limit: with `userland-proxy: false` there is no docker-proxy listener behind a
// published port (iptables does the routing), so an unrelated local process on that port
// matches the publish and is shown as the container. Verifying the listener's comm would
// fix that case but break the legitimate ones (rootless docker, Docker Desktop), so the
// mismatch is documented in the README rather than guessed at here.
fn docker_lookup(
    map: &DockerMap,
    proto: &'static str,
    port: u32,
    local_addr: &str,
) -> Option<DockerInfo> {
    map.get(&(proto, port))?
        .iter()
        .find(|d| ip_matches(&d.host_ip, local_addr))
        .cloned()
}

fn load_docker_ports() -> DockerMap {
    let mut map: DockerMap = HashMap::new();
    let output = match Command::new("docker")
        .args([
            "ps",
            "--format",
            "{{.Names}}\t{{.Ports}}\t{{.Label \"com.docker.compose.project.working_dir\"}}\t{{.Image}}\t{{.RunningFor}}\t{{.Label \"com.docker.compose.project\"}}",
        ])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return map,
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let mut parts = line.splitn(6, '\t');
        let Some(name) = parts.next() else { continue };
        let Some(ports) = parts.next() else { continue };
        let work_dir = parts
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let image = parts.next().unwrap_or("-").to_string();
        let running_for = parts.next().unwrap_or("-").to_string();
        let compose_project = parts
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        for segment in ports.split(',') {
            let seg = segment.trim();
            let Some(arrow) = seg.find("->") else { continue };
            let left = &seg[..arrow];
            let right = &seg[arrow + 2..];
            // right looks like "80/tcp" or "80-82/udp"
            let mut right_parts = right.split('/');
            let cport_str = right_parts.next().unwrap_or("");
            let proto: &'static str = match right_parts.next().unwrap_or("") {
                "tcp" => "tcp",
                "udp" => "udp",
                _ => continue,
            };
            let Some(colon) = left.rfind(':') else { continue };
            let port_str = &left[colon + 1..];
            let host_ip = split_bind_addr(left);
            let (start, end) = match parse_port_range(port_str) {
                Some(r) => r,
                None => continue,
            };
            let (cstart, _cend) = parse_port_range(cport_str).unwrap_or((start, end));
            for (i, p) in (start..=end).enumerate() {
                let cp = cstart + i as u32;
                map.entry((proto, p)).or_default().push(DockerInfo {
                    name: name.to_string(),
                    image: image.clone(),
                    running_for: running_for.clone(),
                    work_dir: work_dir.clone(),
                    container_port: cp,
                    host_ip: host_ip.to_string(),
                    compose_project: compose_project.clone(),
                });
            }
        }
    }
    map
}

fn parse_port_range(s: &str) -> Option<(u32, u32)> {
    if let Some(dash) = s.find('-') {
        let a = s[..dash].parse::<u32>().ok()?;
        let b = s[dash + 1..].parse::<u32>().ok()?;
        Some((a, b))
    } else {
        let p = s.parse::<u32>().ok()?;
        Some((p, p))
    }
}

// ================================================================
// Listening port collection (platform-specific)
// ================================================================

// `strict` distinguishes the primary collection — where a failing `ss` must fail
// loudly rather than masquerade as "no ports" — from the best-effort re-scan
// `warn_on_restart` runs after a kill: that re-scan is only there to add a warning,
// so a transient `ss` failure must not flip an already-successful kill to exit 1.
#[cfg(target_os = "linux")]
fn collect_listening(docker_map: &DockerMap, strict: bool) -> Vec<Entry> {
    let mut entries = Vec::new();
    collect_ss("tcp", &["-tlnpH"], docker_map, &mut entries, strict);
    collect_ss("udp", &["-ulnpH"], docker_map, &mut entries, strict);
    entries
}

#[cfg(target_os = "linux")]
fn collect_ss(
    proto: &'static str,
    args: &[&str],
    docker_map: &DockerMap,
    out: &mut Vec<Entry>,
    strict: bool,
) {
    let output = match Command::new("ss").args(args).output() {
        Ok(o) => o,
        Err(e) => {
            if strict {
                eprintln!("error: failed to run `ss`: {}. Install iproute2 (provides `ss`).", e);
                std::process::exit(1);
            }
            return;
        }
    };
    // busybox `ss` does not support `-H` and exits non-zero instead of printing
    // rows. Parsing its empty stdout would report "no listening ports" — a wrong
    // answer that looks like a right one.
    if !output.status.success() {
        if strict {
            eprintln!(
                "error: `ss {}` failed (exit {}): {}",
                args.join(" "),
                output
                    .status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".into()),
                String::from_utf8_lossy(&output.stderr).trim()
            );
            std::process::exit(1);
        }
        return;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        parse_ss_line(line, proto, docker_map, out);
    }
}

#[cfg(target_os = "linux")]
fn parse_ss_line(
    line: &str,
    proto: &'static str,
    docker_map: &DockerMap,
    out: &mut Vec<Entry>,
) {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 4 {
        return;
    }
    let local = fields[3];
    let Some(port_str) = local.rsplit(':').next() else {
        return;
    };
    let port_str = port_str.trim_end_matches(']');
    let Some(port) = port_str.parse::<u32>().ok().filter(|&p| p > 0) else {
        return;
    };

    let docker = docker_lookup(docker_map, proto, port, split_bind_addr(local));
    let addr = display_bind_addr(local).to_string();

    // `users:(...)` is the trailing column. Locate it in the raw line instead
    // of in whitespace-split tokens, because the process name inside the
    // parens can itself contain spaces (e.g. comm `next-server (v1`), which
    // would otherwise tear the token apart and hide the PID.
    let users_field = line.find("users:").map(|i| &line[i..]);
    let pairs = match users_field {
        Some(s) => parse_users(s),
        None => Vec::new(),
    };

    if pairs.is_empty() {
        out.push(Entry {
            proto,
            port,
            pid: None,
            process: "?".to_string(),
            addr,
            cwd: String::new(),
            cmdline: String::new(),
            parent: None,
            docker,
            stats: Stats::default(),
            user_launched: false,
        });
        return;
    }

    for (name, pid) in pairs {
        out.push(Entry {
            proto,
            port,
            pid: Some(pid),
            process: name,
            addr: addr.clone(),
            cwd: String::new(),
            cmdline: String::new(),
            parent: None,
            docker: docker.clone(),
            stats: Stats::default(),
            user_launched: false,
        });
    }
}

#[cfg(target_os = "linux")]
fn parse_users(s: &str) -> Vec<(String, u32)> {
    // users field looks like: users:(("name1",pid=123,fd=10),("name2",pid=456,fd=11))
    // Each entry is ("comm",pid=N,fd=M). ss does not escape quotes inside comm, so the
    // next `"` is not a reliable terminator — the literal `",pid=` sequence is. A comm
    // containing that sequence itself is unparseable and accepted as such.
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(open) = rest.find("(\"") {
        let after = &rest[open + 2..];
        let Some(end) = after.find("\",pid=") else { break };
        let name = &after[..end];
        let digits = &after[end + 6..];
        let digits_end = digits
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(digits.len());
        if let Ok(pid) = digits[..digits_end].parse::<u32>() {
            out.push((name.to_string(), pid));
        }
        rest = &digits[digits_end..];
    }
    out
}

// `_strict` is unused on macOS: `lsof` exits non-zero when nothing matches, which is
// normal, so this path never exits on a non-zero status to begin with.
//
// The `f` (fd) field is requested not for its value but as a per-socket delimiter: the
// state machine flushes one Entry per `f` record. lsof 4.94+ (the lsof-org fork, what
// Homebrew ships) only emits fields named in `-F`, so `f` must be listed explicitly —
// without it every listener is silently dropped.
#[cfg(target_os = "macos")]
fn collect_listening(docker_map: &DockerMap, _strict: bool) -> Vec<Entry> {
    let mut entries = Vec::new();
    collect_lsof(
        "tcp",
        &["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpcnf"],
        docker_map,
        &mut entries,
    );
    collect_lsof("udp", &["-nP", "-iUDP", "-Fpcnf"], docker_map, &mut entries);
    entries
}

#[cfg(target_os = "macos")]
fn collect_lsof(
    proto: &'static str,
    args: &[&str],
    docker_map: &DockerMap,
    out: &mut Vec<Entry>,
) {
    let output = match Command::new("lsof").args(args).output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: failed to run `lsof`: {}.", e);
            std::process::exit(1);
        }
    };
    // lsof exits non-zero when nothing matches; that's fine, parse whatever is there.
    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut cur_pid: Option<u32> = None;
    let mut cur_cmd: String = "?".to_string();
    let mut has_file = false;
    let mut cur_name: Option<String> = None;

    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        // A value with an embedded newline spills untagged continuation lines;
        // drop them (a multi-byte first char would make split_at(1) panic).
        let Some((tag, val)) = line.split_at_checked(1) else { continue };
        match tag {
            "p" => {
                if has_file {
                    flush_lsof(proto, cur_pid, &cur_cmd, &cur_name, docker_map, out);
                }
                has_file = false;
                cur_name = None;
                cur_pid = val.parse().ok();
                cur_cmd = "?".to_string();
            }
            "c" => {
                cur_cmd = val.to_string();
            }
            "f" => {
                if has_file {
                    flush_lsof(proto, cur_pid, &cur_cmd, &cur_name, docker_map, out);
                }
                has_file = true;
                cur_name = None;
            }
            "n" => {
                cur_name = Some(val.to_string());
            }
            _ => {}
        }
    }
    if has_file {
        flush_lsof(proto, cur_pid, &cur_cmd, &cur_name, docker_map, out);
    }
}

#[cfg(target_os = "macos")]
fn flush_lsof(
    proto: &'static str,
    pid: Option<u32>,
    cmd: &str,
    name: &Option<String>,
    docker_map: &DockerMap,
    out: &mut Vec<Entry>,
) {
    let Some(name) = name else { return };
    // Strip trailing " (LISTEN)" or similar parenthetical suffixes
    let name_first = name.split_whitespace().next().unwrap_or(name);
    // A connected socket looks like "local:port->remote:port"; that is not a
    // listener. `lsof -iUDP` (which has no LISTEN filter) can surface those,
    // and their trailing segment would be parsed as a local port otherwise.
    if name_first.contains("->") {
        return;
    }
    // For UDP with an idle state (e.g. "*:5353" or "*:5353 (IDLE)") we still want the addr part.
    let Some(port_str) = name_first.rsplit(':').next() else {
        return;
    };
    let port_str = port_str.trim_end_matches(']');
    let Ok(port) = port_str.parse::<u32>() else {
        return;
    };
    if port == 0 {
        return;
    }
    let docker = docker_lookup(docker_map, proto, port, split_bind_addr(name_first));
    out.push(Entry {
        proto,
        port,
        pid,
        process: cmd.to_string(),
        addr: display_bind_addr(name_first).to_string(),
        cwd: String::new(),
        cmdline: String::new(),
        parent: None,
        docker,
        stats: Stats::default(),
        user_launched: false,
    });
}

// ================================================================
// CWD / cmdline / user-launched detection (platform-specific)
// ================================================================

#[cfg(target_os = "linux")]
fn enrich_process_info(entries: &mut [Entry]) {
    for e in entries.iter_mut() {
        if e.pid.is_none() {
            e.cwd = "-".to_string();
            e.cmdline = "-".to_string();
        }
    }

    let unique_pids: HashSet<u32> = entries.iter().filter_map(|e| e.pid).collect();
    if unique_pids.is_empty() {
        return;
    }

    // Read /proc entries once per PID. A single PID can listen on many ports
    // (a reverse proxy, for example), and the previous implementation paid
    // those syscalls per-entry.
    let mut info: HashMap<u32, (String, String, bool)> = HashMap::with_capacity(unique_pids.len());
    for pid in &unique_pids {
        let cwd = read_cwd_proc(*pid);
        let cmdline = read_cmdline_proc(*pid);
        let user_launched = read_user_launched_proc(*pid);
        info.insert(*pid, (cwd, cmdline, user_launched));
    }

    for e in entries.iter_mut() {
        if let Some(pid) = e.pid {
            if let Some((cwd, cmdline, user_launched)) = info.get(&pid) {
                e.cwd = cwd.clone();
                e.cmdline = cmdline.clone();
                e.user_launched = *user_launched;
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn read_cwd_proc(pid: u32) -> String {
    fs::read_link(format!("/proc/{}/cwd", pid))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "?".to_string())
}

#[cfg(target_os = "linux")]
fn read_cmdline_proc(pid: u32) -> String {
    match fs::read(format!("/proc/{}/cmdline", pid)) {
        Ok(mut bytes) => {
            for b in bytes.iter_mut() {
                if *b == 0 {
                    *b = b' ';
                }
            }
            let s = String::from_utf8_lossy(&bytes).trim().to_string();
            if s.is_empty() {
                "-".to_string()
            } else {
                s
            }
        }
        Err(_) => "?".to_string(),
    }
}

#[cfg(target_os = "linux")]
fn read_has_tty_proc(pid: u32) -> bool {
    let Ok(stat) = fs::read_to_string(format!("/proc/{}/stat", pid)) else {
        return false;
    };
    let Some(rparen) = stat.rfind(')') else {
        return false;
    };
    let rest = &stat[rparen + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // after comm: state(0) ppid(1) pgrp(2) session(3) tty_nr(4)
    let Some(tty) = fields.get(4).and_then(|s| s.parse::<i32>().ok()) else {
        return false;
    };
    tty != 0
}

#[cfg(target_os = "linux")]
fn read_exe_basename_proc(pid: u32) -> Option<String> {
    let path = fs::read_link(format!("/proc/{}/exe", pid)).ok()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    // The kernel appends " (deleted)" once the binary is unlinked (a rebuild or a
    // package upgrade under a running server); the marker is not part of the name.
    Some(
        name.strip_suffix(" (deleted)")
            .map(str::to_string)
            .unwrap_or(name),
    )
}

#[cfg(target_os = "linux")]
fn read_user_launched_proc(pid: u32) -> bool {
    if read_has_tty_proc(pid) {
        return true;
    }
    read_exe_basename_proc(pid)
        .as_deref()
        .map(is_interpreter_exe)
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn read_ppid_proc(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
    // comm can contain spaces/parens, so start after the last ')': state(0) ppid(1) ...
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse::<u32>().ok()
}

// (ppid, parent's cmdline). Reads /proc directly; only called in info mode.
#[cfg(target_os = "linux")]
fn read_parent_info(pid: u32) -> Option<(u32, String)> {
    let ppid = read_ppid_proc(pid)?;
    let cmd = read_cmdline_proc(ppid);
    // cmdline is empty for kernel threads and unreadable across privilege boundaries;
    // comm is a usable fallback in both cases.
    let cmd = if matches!(cmd.as_str(), "-" | "?") {
        fs::read_to_string(format!("/proc/{}/comm", ppid))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or(cmd)
    } else {
        cmd
    };
    Some((ppid, cmd))
}

#[cfg(target_os = "linux")]
fn enrich_parent_info(entries: &mut [Entry]) {
    let pids: HashSet<u32> = entries.iter().filter_map(|e| e.pid).collect();
    let mut info: HashMap<u32, (u32, String)> = HashMap::with_capacity(pids.len());
    for pid in &pids {
        if let Some(p) = read_parent_info(*pid) {
            info.insert(*pid, p);
        }
    }
    for e in entries.iter_mut() {
        if let Some(pid) = e.pid {
            e.parent = info.get(&pid).cloned();
        }
    }
}

#[cfg(target_os = "macos")]
fn enrich_parent_info(entries: &mut [Entry]) {
    let pids: HashSet<u32> = entries.iter().filter_map(|e| e.pid).collect();
    if pids.is_empty() {
        return;
    }
    let pid_list = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");

    // pid -> ppid
    let mut ppid_of: HashMap<u32, u32> = HashMap::new();
    if let Ok(output) = Command::new("ps")
        .args(["-o", "pid=,ppid=", "-p", &pid_list])
        .output()
    {
        let s = String::from_utf8_lossy(&output.stdout);
        for line in s.lines() {
            let mut it = line.split_whitespace();
            if let (Some(p), Some(pp)) = (it.next(), it.next()) {
                if let (Ok(p), Ok(pp)) = (p.parse::<u32>(), pp.parse::<u32>()) {
                    ppid_of.insert(p, pp);
                }
            }
        }
    }
    if ppid_of.is_empty() {
        return;
    }

    // ppid -> command line
    let ppid_list = ppid_of
        .values()
        .collect::<HashSet<_>>()
        .into_iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut cmd_of: HashMap<u32, String> = HashMap::new();
    if let Ok(output) = Command::new("ps")
        .args(["-ww", "-o", "pid=,command=", "-p", &ppid_list])
        .output()
    {
        let s = String::from_utf8_lossy(&output.stdout);
        for line in s.lines() {
            let trimmed = line.trim_start();
            let Some(sp) = trimmed.find(char::is_whitespace) else {
                continue;
            };
            let (pid_s, rest) = trimmed.split_at(sp);
            if let Ok(pp) = pid_s.parse::<u32>() {
                cmd_of.insert(pp, rest.trim().to_string());
            }
        }
    }

    for e in entries.iter_mut() {
        if let Some(pid) = e.pid {
            if let Some(&ppid) = ppid_of.get(&pid) {
                let cmd = cmd_of
                    .get(&ppid)
                    .filter(|c| !c.is_empty())
                    .cloned()
                    .unwrap_or_else(|| "?".to_string());
                e.parent = Some((ppid, cmd));
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn enrich_process_info(entries: &mut [Entry]) {
    for e in entries.iter_mut() {
        if e.pid.is_none() {
            e.cwd = "-".to_string();
            e.cmdline = "-".to_string();
        }
    }

    let unique_pids: HashSet<u32> = entries.iter().filter_map(|e| e.pid).collect();
    if unique_pids.is_empty() {
        return;
    }
    let pid_list = unique_pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");

    // ps pass 1: tty + comm (exe path) per pid.
    // BSD ps uses "??" to mean "no controlling tty".
    let mut tty_exe: HashMap<u32, (bool, String)> = HashMap::new();
    if let Ok(output) = Command::new("ps")
        .args(["-ww", "-o", "pid=,tty=,comm=", "-p", &pid_list])
        .output()
    {
        let s = String::from_utf8_lossy(&output.stdout);
        for line in s.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 3 {
                continue;
            }
            let Ok(pid) = parts[0].parse::<u32>() else {
                continue;
            };
            let tty = parts[1];
            let has_tty = tty != "??" && tty != "?" && tty != "-";
            // comm may contain spaces (paths with spaces); join the remainder.
            let comm = parts[2..].join(" ");
            let basename = comm.rsplit('/').next().unwrap_or(&comm).to_string();
            tty_exe.insert(pid, (has_tty, basename));
        }
    }

    // ps pass 2: full command line per pid.
    let mut cmdline_map: HashMap<u32, String> = HashMap::new();
    if let Ok(output) = Command::new("ps")
        .args(["-ww", "-o", "pid=,command=", "-p", &pid_list])
        .output()
    {
        let s = String::from_utf8_lossy(&output.stdout);
        for line in s.lines() {
            let trimmed = line.trim_start();
            let Some(sp) = trimmed.find(char::is_whitespace) else {
                continue;
            };
            let (pid_s, rest) = trimmed.split_at(sp);
            let Ok(pid) = pid_s.parse::<u32>() else {
                continue;
            };
            let cmd = rest.trim();
            let value = if cmd.is_empty() { "-".to_string() } else { cmd.to_string() };
            cmdline_map.insert(pid, value);
        }
    }

    // lsof pass: cwd per pid, batched.
    let mut cwd_map: HashMap<u32, String> = HashMap::new();
    if let Ok(output) = Command::new("lsof")
        .args(["-a", "-p", &pid_list, "-d", "cwd", "-Fn"])
        .output()
    {
        let s = String::from_utf8_lossy(&output.stdout);
        let mut cur_pid: Option<u32> = None;
        for line in s.lines() {
            if line.is_empty() {
                continue;
            }
            // A value with an embedded newline spills untagged continuation lines;
            // drop them (a multi-byte first char would make split_at(1) panic).
            let Some((tag, val)) = line.split_at_checked(1) else { continue };
            match tag {
                "p" => cur_pid = val.parse().ok(),
                "n" => {
                    if let Some(pid) = cur_pid {
                        cwd_map.entry(pid).or_insert_with(|| val.to_string());
                    }
                }
                _ => {}
            }
        }
    }

    for e in entries.iter_mut() {
        if let Some(pid) = e.pid {
            e.cwd = cwd_map.get(&pid).cloned().unwrap_or_else(|| "?".to_string());
            e.cmdline = cmdline_map
                .get(&pid)
                .cloned()
                .unwrap_or_else(|| "?".to_string());
            e.user_launched = tty_exe
                .get(&pid)
                .map(|(has_tty, exe)| *has_tty || is_interpreter_exe(exe))
                .unwrap_or(false);
        }
    }
}

// ================================================================
// Stats enrichment (ps; cross-platform with cfg-gated format)
// ================================================================

fn enrich_local_stats(entries: &mut [Entry]) {
    let pids: Vec<u32> = entries
        .iter()
        .filter(|e| e.docker.is_none())
        .filter_map(|e| e.pid)
        .collect();
    if pids.is_empty() {
        return;
    }
    let pid_arg = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");

    // BSD ps on macOS does not have `nlwp` (thread count); skip that column.
    let fmt = if cfg!(target_os = "macos") {
        "pid=,pcpu=,rss=,etime=,user="
    } else {
        "pid=,pcpu=,rss=,nlwp=,etime=,user="
    };
    let output = match Command::new("ps").args(["-o", fmt, "-p", &pid_arg]).output() {
        Ok(o) => o,
        _ => return,
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut map: HashMap<u32, Stats> = HashMap::new();
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let min_len = if cfg!(target_os = "macos") { 5 } else { 6 };
        if parts.len() < min_len {
            continue;
        }
        let Ok(pid) = parts[0].parse::<u32>() else {
            continue;
        };
        let cpu = format!("{}%", parts[1]);
        let rss_kb: u64 = parts[2].parse().unwrap_or(0);
        let mem = format_mem(rss_kb * 1024);
        let (threads, etime_idx, user_idx) = if cfg!(target_os = "macos") {
            (None::<u32>, 3usize, 4usize)
        } else {
            (parts[3].parse::<u32>().ok(), 4usize, 5usize)
        };
        let uptime = format_etime(parts[etime_idx]);
        let user = Some(parts[user_idx].to_string());
        map.insert(
            pid,
            Stats {
                cpu,
                mem,
                uptime,
                threads,
                user,
            },
        );
    }
    for e in entries.iter_mut() {
        if e.docker.is_none() {
            if let Some(pid) = e.pid {
                if let Some(s) = map.get(&pid) {
                    e.stats = s.clone();
                }
            }
        }
    }
}

fn enrich_docker_stats(entries: &mut [Entry], include_cpu_mem: bool) {
    for e in entries.iter_mut() {
        if let Some(d) = &e.docker {
            e.stats.uptime = d.running_for.trim_end_matches(" ago").to_string();
        }
    }
    if !include_cpu_mem {
        return;
    }
    let mut seen: HashSet<String> = HashSet::new();
    let names: Vec<String> = entries
        .iter()
        .filter_map(|e| e.docker.as_ref().map(|d| d.name.clone()))
        .filter(|n| seen.insert(n.clone()))
        .collect();
    if names.is_empty() {
        return;
    }
    let run_stats = |names: &[String]| {
        let mut cmd = Command::new("docker");
        cmd.args([
            "stats",
            "--no-stream",
            "--format",
            "{{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}",
        ]);
        for n in names {
            cmd.arg(n);
        }
        match cmd.output() {
            Ok(o) if o.status.success() => Some(o),
            _ => None,
        }
    };
    // `docker stats` fails as a batch — exit 1, empty stdout — when any name in it is
    // already gone, which is a live possibility in the second between our `docker ps`
    // and this call. Losing CPU/MEM for every container because one died is worse than
    // one extra `docker ps`, so retry once with the names that are still up.
    let output = match run_stats(&names) {
        Some(o) => o,
        None => {
            let live: HashSet<String> = match Command::new("docker")
                .args(["ps", "--format", "{{.Names}}"])
                .output()
            {
                Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(str::to_string)
                    .collect(),
                _ => return,
            };
            let still_up: Vec<String> = names.into_iter().filter(|n| live.contains(n)).collect();
            if still_up.is_empty() {
                return;
            }
            match run_stats(&still_up) {
                Some(o) => o,
                None => return,
            }
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut map: HashMap<String, (String, String)> = HashMap::new();
    for line in stdout.lines() {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() < 3 {
            continue;
        }
        let name = parts[0].to_string();
        let cpu = parts[1].to_string();
        let mem = parts[2]
            .split('/')
            .next()
            .unwrap_or("-")
            .trim()
            .to_string();
        map.insert(name, (cpu, mem));
    }
    for e in entries.iter_mut() {
        if let Some(d) = &e.docker {
            if let Some((cpu, mem)) = map.get(&d.name) {
                e.stats.cpu = cpu.clone();
                e.stats.mem = mem.clone();
            }
        }
    }
}

fn format_mem(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.1}G", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.0}M", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{}K", bytes / KB)
    } else {
        format!("{}B", bytes)
    }
}

fn format_etime(et: &str) -> String {
    let (days, rest) = match et.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().unwrap_or(0), r),
        None => (0, et),
    };
    let parts: Vec<&str> = rest.split(':').collect();
    let (h, m, s): (u64, u64, u64) = match parts.len() {
        3 => (
            parts[0].parse().unwrap_or(0),
            parts[1].parse().unwrap_or(0),
            parts[2].parse().unwrap_or(0),
        ),
        2 => (0, parts[0].parse().unwrap_or(0), parts[1].parse().unwrap_or(0)),
        1 => (0, 0, parts[0].parse().unwrap_or(0)),
        _ => return et.to_string(),
    };
    let total = days * 86400 + h * 3600 + m * 60 + s;
    if total >= 86400 {
        format!("{}d{}h", total / 86400, (total % 86400) / 3600)
    } else if total >= 3600 {
        format!("{}h{}m", total / 3600, (total % 3600) / 60)
    } else if total >= 60 {
        format!("{}m{}s", total / 60, total % 60)
    } else {
        format!("{}s", total)
    }
}

// Visual group key: compose project for docker rows in the same project,
// container name when no compose label, cwd for local rows. Two entries with
// the same key sit in the same visual block in the table.
fn group_key(e: &Entry) -> &str {
    if let Some(d) = &e.docker {
        d.compose_project.as_deref().unwrap_or(d.name.as_str())
    } else {
        e.cwd.as_str()
    }
}

// ================================================================
// Update notice (best-effort, never blocks the dashboard)
// ================================================================
//
// Strategy: cache the latest upstream version under
// `${XDG_CACHE_HOME:-$HOME/.cache}/lport/update-check`. On every run:
//   1. Read the cache. If a newer version is recorded, print a one-line
//      notice to stderr (after the table).
//   2. If the cache is older than 24h (or missing), spawn a detached `sh`
//      that re-fetches `Cargo.toml` from main and rewrites the cache. The
//      result is observable on the *next* run, not this one — so startup
//      stays fast.
// `LPORT_NO_UPDATE_CHECK=1` disables both halves.

const UPDATE_CACHE_TTL_SECS: u64 = 24 * 3600;
const UPDATE_REPO: &str = "https://github.com/Changroro/lport";
const UPDATE_RAW_CARGO_TOML: &str =
    "https://raw.githubusercontent.com/Changroro/lport/main/Cargo.toml";

fn maybe_print_update_notice() {
    if std::env::var_os("LPORT_NO_UPDATE_CHECK").is_some() {
        return;
    }
    let Some(cache_path) = update_cache_path() else {
        return;
    };
    let current = env!("CARGO_PKG_VERSION");

    let cached = std::fs::read_to_string(&cache_path).ok();
    let mut cache_fresh = false;
    let mut latest: Option<String> = None;
    if let Some(s) = cached.as_deref() {
        let mut lines = s.lines();
        let ts_line = lines.next().unwrap_or("").trim();
        let ver_line = lines.next().unwrap_or("").trim();
        if let (Ok(ts), Some(now)) = (ts_line.parse::<u64>(), unix_now()) {
            if now >= ts && now - ts < UPDATE_CACHE_TTL_SECS {
                cache_fresh = true;
            }
        }
        if !ver_line.is_empty() && version_gt(ver_line, current) {
            latest = Some(ver_line.to_string());
        }
    }
    if let Some(latest) = latest {
        prompt_or_print_update(current, &latest);
    }
    if !cache_fresh {
        spawn_update_cache_refresh(&cache_path);
    }
}

// Show the update notice. If the session is fully interactive (stdin, stdout,
// stderr all attached to a TTY), also prompt `[y/N]` and run
// `cargo install --git ... --force` on `y`. Default is N so an unattended
// Enter does not silently kick off a heavy rebuild.
fn prompt_or_print_update(current: &str, latest: &str) {
    let stderr_tty = io::stderr().is_terminal();
    let interactive =
        stderr_tty && io::stdout().is_terminal() && io::stdin().is_terminal();
    let dot = if stderr_tty { "\x1b[33m●\x1b[0m" } else { "*" };
    let bold = |s: &str| -> String {
        if stderr_tty {
            format!("\x1b[1m{}\x1b[0m", s)
        } else {
            s.to_string()
        }
    };

    if interactive {
        eprintln!();
        eprint!(
            "{}  update available: lport {} → {}   install now? {} ",
            dot,
            current,
            latest,
            bold("[y/N]"),
        );
        let _ = io::stderr().flush();
        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_ok() {
            let answer = input.trim();
            if answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes") {
                run_cargo_install();
            }
        }
    } else {
        eprintln!();
        eprintln!(
            "{}  update available: lport {} → {}",
            dot, current, latest
        );
        eprintln!(
            "   install: cargo install --git {} --force",
            UPDATE_REPO
        );
    }
}

fn run_cargo_install() {
    eprintln!();
    eprintln!("==> cargo install --git {} --force", UPDATE_REPO);
    match Command::new("cargo")
        .args(["install", "--git", UPDATE_REPO, "--force"])
        .status()
    {
        Ok(s) if s.success() => {
            eprintln!("==> lport updated. Re-run to use the new version.");
        }
        Ok(s) => {
            eprintln!(
                "==> cargo install exited with status {}.",
                s.code().map(|c| c.to_string()).unwrap_or_else(|| "?".into())
            );
        }
        Err(e) => {
            eprintln!("==> failed to spawn cargo: {}.", e);
            eprintln!("    is the Rust toolchain installed and on PATH?");
        }
    }
}

fn update_cache_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("lport").join("update-check"))
}

fn unix_now() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

// Numeric semver-ish comparison. `0.10.0` > `0.9.0`. Pre-release suffixes
// after `-` are stripped before comparison.
fn version_gt(a: &str, b: &str) -> bool {
    fn parse(v: &str) -> Vec<u32> {
        v.trim()
            .trim_start_matches('v')
            .split('-')
            .next()
            .unwrap_or("")
            .split('.')
            .map(|s| s.parse::<u32>().unwrap_or(0))
            .collect()
    }
    parse(a) > parse(b)
}

fn spawn_update_cache_refresh(cache_path: &std::path::Path) {
    let dir = match cache_path.parent() {
        Some(d) => d.to_string_lossy().into_owned(),
        None => return,
    };
    let cache_str = cache_path.to_string_lossy().into_owned();
    // Both paths are quoted with single quotes; embedded single quotes get
    // the standard `'\''` escape so the shell receives the literal path.
    let dir_q = dir.replace('\'', r"'\''");
    let cache_q = cache_str.replace('\'', r"'\''");
    let script = format!(
        "v=$(curl -fsSL --max-time 5 {url} 2>/dev/null \
            | awk -F'\"' '/^version[[:space:]]*=/ {{print $2; exit}}'); \
         if [ -n \"$v\" ]; then \
            mkdir -p '{dir}' && printf '%s\\n%s\\n' \"$(date +%s)\" \"$v\" > '{cache}'; \
         fi",
        url = UPDATE_RAW_CARGO_TOML,
        dir = dir_q,
        cache = cache_q,
    );
    // Detach: stdio piped to /dev/null. When this process exits, the child
    // is reparented to init and continues. No wait() is needed.
    let _ = Command::new("sh")
        .arg("-c")
        .arg(&script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

// Terminal cells occupied by `s`. The wide set below is an approximation of the
// East Asian Wide/Fullwidth ranges (plus the emoji blocks and the Emoji_Presentation
// defaults), not a full UAX #11 table — enough to keep columns aligned for the paths,
// process names and container names lport prints.
fn display_width(s: &str) -> usize {
    s.chars().map(|c| if is_wide_char(c) { 2 } else { 1 }).sum()
}

fn is_wide_char(c: char) -> bool {
    matches!(
        c as u32,
        0x1100..=0x115F
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE10..=0xFE19
            | 0xFE30..=0xFE6F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x1F300..=0x1F64F
            | 0x1F680..=0x1F6FF
            | 0x1F7E0..=0x1F7EB
            | 0x1F900..=0x1F9FF
            | 0x1FA70..=0x1FAFF
            | 0x20000..=0x2FFFD
            | 0x30000..=0x3FFFD
            // Emoji_Presentation defaults scattered through U+2xxx: listed one by one
            // because their neighbours (☑ ⚠ …) default to text presentation and stay narrow.
            | 0x231A..=0x231B
            | 0x23E9..=0x23EC
            | 0x23F0
            | 0x23F3
            | 0x25FD..=0x25FE
            | 0x2614..=0x2615
            | 0x2648..=0x2653
            | 0x267F
            | 0x2693
            | 0x26A1
            | 0x26AA..=0x26AB
            | 0x26BD..=0x26BE
            | 0x26C4..=0x26C5
            | 0x26CE
            | 0x26D4
            | 0x26EA
            | 0x26F2..=0x26F3
            | 0x26F5
            | 0x26FA
            | 0x26FD
            | 0x2705
            | 0x270A..=0x270B
            | 0x2728
            | 0x274C
            | 0x274E
            | 0x2753..=0x2755
            | 0x2757
            | 0x2795..=0x2797
            | 0x27B0
            | 0x27BF
            | 0x2B1B..=0x2B1C
            | 0x2B50
            | 0x2B55
    )
}

fn nz(s: &str) -> String {
    if s.is_empty() {
        "-".to_string()
    } else {
        s.to_string()
    }
}

fn shorten_path(s: &str, home: Option<&str>) -> String {
    // Truncation is intentionally absent: the CWD is the feature, clipping it
    // to `…` would defeat the point. Only the `$HOME -> ~` substitution is
    // applied here for readability; columns expand to whatever width is needed.
    if matches!(s, "" | "-" | "?" | "/") {
        return s.to_string();
    }
    if let Some(h) = home {
        if s == h {
            return "~".to_string();
        }
        let prefix = format!("{}/", h);
        if let Some(rest) = s.strip_prefix(&prefix) {
            return format!("~/{}", rest);
        }
    }
    s.to_string()
}

fn print_info(entries: &[Entry], ports: &[u32]) -> i32 {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    // Report each requested port that has no listener, the way `kill` does. A single
    // "(no matching port found)" hid which port of `lport info 5432 59321` was empty.
    let mut missing = false;
    for &port in ports {
        if !entries.iter().any(|e| e.port == port) {
            eprintln!("port {}: no listening process found.", port);
            missing = true;
        }
    }

    // Soft writes: `info` owes the caller an exit code (a missing port is 1), and exiting
    // 0 on a closed pipe would silently break that contract. There is nothing to gain from
    // bailing out early either — `info` prints a handful of rows, not a whole system.
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            outln_soft!(out, "");
        }
        outln_soft!(out, "─────────────────────────────────────────────");
        let rows: Vec<(&str, String)> = match &e.docker {
            Some(d) => docker_info_rows(e, d),
            None => local_info_rows(e),
        };
        let label_w = rows.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
        for (label, value) in &rows {
            outln_soft!(out, "  {:<width$}  {}", label, value, width = label_w);
        }
    }
    if missing { 1 } else { 0 }
}

fn local_info_rows(e: &Entry) -> Vec<(&'static str, String)> {
    let mut rows: Vec<(&'static str, String)> = vec![
        ("PORT", format!("{}/{}", e.proto, e.port)),
        ("ADDR", nz(&e.addr)),
        ("PROCESS", e.process.clone()),
        (
            "PID",
            e.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
        ),
    ];
    if let Some(u) = &e.stats.user {
        rows.push(("USER", u.clone()));
    }
    rows.push(("CPU", nz(&e.stats.cpu)));
    rows.push(("MEM", nz(&e.stats.mem)));
    if let Some(t) = e.stats.threads {
        rows.push(("THREADS", t.to_string()));
    }
    rows.push(("UPTIME", nz(&e.stats.uptime)));
    if let Some((ppid, cmd)) = &e.parent {
        rows.push(("PARENT", format!("pid {} ({})", ppid, cmd)));
    }
    rows.push(("CWD", e.cwd.clone()));
    rows.push(("CMD", e.cmdline.clone()));
    rows
}

fn docker_info_rows(e: &Entry, d: &DockerInfo) -> Vec<(&'static str, String)> {
    let mut rows: Vec<(&'static str, String)> = vec![
        (
            "PORT",
            format!("{}/{} → {} (in container)", e.proto, e.port, d.container_port),
        ),
        ("ADDR", nz(&e.addr)),
        ("TYPE", "docker container".to_string()),
        ("CONTAINER", d.name.clone()),
        ("IMAGE", d.image.clone()),
    ];
    if let Some(w) = &d.work_dir {
        rows.push(("WORKDIR", w.clone()));
    }
    rows.push(("CPU", nz(&e.stats.cpu)));
    rows.push(("MEM", nz(&e.stats.mem)));
    rows.push(("UPTIME", nz(&e.stats.uptime)));
    rows
}

fn print_table(entries: &[Entry], dev_mode: bool) {
    let headers: &[&str] = &[
        "PROTO", "PORT", "PID", "PROCESS", "JOB", "CPU", "MEM", "UPTIME",
    ];

    let home = std::env::var("HOME").ok();

    let rows: Vec<Vec<String>> = entries
        .iter()
        .map(|e| {
            let (process, job) = match &e.docker {
                Some(d) => ("docker".to_string(), d.name.clone()),
                None => (e.process.clone(), shorten_path(&e.cwd, home.as_deref())),
            };
            vec![
                e.proto.to_string(),
                e.port.to_string(),
                e.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".to_string()),
                process,
                job,
                nz(&e.stats.cpu),
                nz(&e.stats.mem),
                nz(&e.stats.uptime),
            ]
        })
        .collect();

    let mut widths: Vec<usize> = headers.iter().map(|h| display_width(h)).collect();
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(display_width(cell));
        }
    }

    // `{:<width$}` pads by char count, which under-pads CJK cells (a Hangul path
    // component occupies two terminal cells per char). Pad by display width.
    let fmt_row = |cells: &[String]| -> String {
        cells
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{}{}", c, " ".repeat(w.saturating_sub(display_width(c)))))
            .collect::<Vec<_>>()
            .join("  ")
    };
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let header_cells: Vec<String> = headers.iter().map(|s| s.to_string()).collect();
    outln!(out, "{}", fmt_row(&header_cells));
    let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    outln!(out, "{}", sep.join("  "));
    // Every docker block (whether the group has one row or many) gets the
    // same `[ name ]` header treatment, preceded by a blank line so it reads
    // as a distinct block from neighbouring rows. Group key is the compose
    // project label if present, else the container name. Local rows stay
    // compact and are never given a separator among themselves.
    let mut prev_docker_group: Option<&str> = None;
    for (i, (e, row)) in entries.iter().zip(rows.iter()).enumerate() {
        let cur_docker_group: Option<&str> = if e.docker.is_some() {
            Some(group_key(e))
        } else {
            None
        };
        if cur_docker_group.is_some() && cur_docker_group != prev_docker_group {
            if i > 0 {
                outln!(out, "");
            }
            if let Some(name) = cur_docker_group {
                outln!(out, "[ {} ]", name);
            }
        }
        outln!(out, "{}", fmt_row(row));
        prev_docker_group = cur_docker_group;
    }

    if entries.is_empty() {
        if dev_mode {
            eprintln!("\n(no listening ports found — try running with sudo)");
        } else {
            eprintln!("\n(no user servers to display — run `lport --dev` to see everything)");
        }
    }
}
