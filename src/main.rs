use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::process::Command;

macro_rules! outln {
    ($out:expr, $($arg:tt)*) => {{
        if let Err(e) = writeln!($out, $($arg)*) {
            if e.kind() == io::ErrorKind::BrokenPipe {
                std::process::exit(0);
            }
        }
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
}

struct Entry {
    proto: &'static str,
    port: u32,
    pid: Option<u32>,
    process: String,
    cwd: String,
    cmdline: String,
    docker: Option<DockerInfo>,
    stats: Stats,
}

enum Mode {
    Dashboard { dev: bool },
    Info { ports: Vec<u32> },
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return;
    }

    let mode = parse_mode(&args);
    let docker_map = load_docker_ports();

    let mut entries = Vec::new();
    collect("tcp", &["-tlnpH"], &docker_map, &mut entries);
    collect("udp", &["-ulnpH"], &docker_map, &mut entries);

    entries.sort_by(|a, b| {
        (a.port, a.proto, a.pid.unwrap_or(0)).cmp(&(b.port, b.proto, b.pid.unwrap_or(0)))
    });
    entries.dedup_by(|a, b| a.port == b.port && a.proto == b.proto && a.pid == b.pid);

    match &mode {
        Mode::Info { ports } => {
            entries.retain(|e| ports.contains(&e.port));
        }
        Mode::Dashboard { dev: false } => {
            entries.retain(|e| e.docker.is_some() || is_real_cwd(&e.cwd));
        }
        Mode::Dashboard { dev: true } => {}
    }

    entries.sort_by(|a, b| {
        let ka = a.docker.as_ref().map(|d| d.name.as_str()).unwrap_or(a.cwd.as_str());
        let kb = b.docker.as_ref().map(|d| d.name.as_str()).unwrap_or(b.cwd.as_str());
        (ka, a.port, a.proto).cmp(&(kb, b.port, b.proto))
    });

    enrich_local_stats(&mut entries);
    let with_docker_cpu_mem = matches!(mode, Mode::Info { .. });
    enrich_docker_stats(&mut entries, with_docker_cpu_mem);

    match mode {
        Mode::Info { .. } => print_info(&entries),
        Mode::Dashboard { dev } => print_table(&entries, dev),
    }
}

fn print_help() {
    println!("Usage: lport [--dev]");
    println!("       lport info PORT [PORT ...]");
    println!();
    println!("  (default)        Show user-launched servers and Docker containers only");
    println!("                   (PROTO PORT PID PROCESS JOB CPU MEM UPTIME)");
    println!("  --dev            Show every listening port, including system daemons");
    println!("  info PORT...     Show full details for the given port(s),");
    println!("                   including Docker container CPU/MEM");
    println!("                   example: lport info 8080 5432");
}

fn parse_mode(args: &[String]) -> Mode {
    if let Some(idx) = args.iter().position(|a| a == "info") {
        let mut ports: Vec<u32> = Vec::new();
        for a in &args[idx + 1..] {
            match a.parse::<u32>() {
                Ok(p) if p > 0 => ports.push(p),
                _ => eprintln!("warning: '{}' is not a valid port number, ignored", a),
            }
        }
        if ports.is_empty() {
            eprintln!("error: 'lport info' requires at least one port number.");
            std::process::exit(2);
        }
        return Mode::Info { ports };
    }
    let dev = args.iter().any(|a| a == "--dev");
    Mode::Dashboard { dev }
}

fn is_real_cwd(cwd: &str) -> bool {
    !matches!(cwd, "" | "/" | "?" | "-")
}

fn load_docker_ports() -> HashMap<u32, DockerInfo> {
    let mut map: HashMap<u32, DockerInfo> = HashMap::new();
    let output = match Command::new("docker")
        .args([
            "ps",
            "--format",
            "{{.Names}}\t{{.Ports}}\t{{.Label \"com.docker.compose.project.working_dir\"}}\t{{.Image}}\t{{.RunningFor}}",
        ])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return map,
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let mut parts = line.splitn(5, '\t');
        let Some(name) = parts.next() else { continue };
        let Some(ports) = parts.next() else { continue };
        let work_dir = parts
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let image = parts.next().unwrap_or("-").to_string();
        let running_for = parts.next().unwrap_or("-").to_string();
        for segment in ports.split(',') {
            let seg = segment.trim();
            let Some(arrow) = seg.find("->") else { continue };
            let left = &seg[..arrow];
            let right = &seg[arrow + 2..];
            let cport_str = right.split('/').next().unwrap_or("");
            let Some(colon) = left.rfind(':') else { continue };
            let port_str = &left[colon + 1..];
            let (start, end) = match parse_port_range(port_str) {
                Some(r) => r,
                None => continue,
            };
            let (cstart, _cend) = parse_port_range(cport_str).unwrap_or((start, end));
            for (i, p) in (start..=end).enumerate() {
                let cp = cstart + i as u32;
                map.insert(
                    p,
                    DockerInfo {
                        name: name.to_string(),
                        image: image.clone(),
                        running_for: running_for.clone(),
                        work_dir: work_dir.clone(),
                        container_port: cp,
                    },
                );
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

fn collect(
    proto: &'static str,
    args: &[&str],
    docker_map: &HashMap<u32, DockerInfo>,
    out: &mut Vec<Entry>,
) {
    let output = match Command::new("ss").args(args).output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: failed to run `ss`: {}. Install iproute2 (provides `ss`).", e);
            std::process::exit(1);
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(e) = parse_line(line, proto, docker_map) {
            out.push(e);
        }
    }
}

fn parse_line(
    line: &str,
    proto: &'static str,
    docker_map: &HashMap<u32, DockerInfo>,
) -> Option<Entry> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 4 {
        return None;
    }
    let local = fields[3];
    let port_str = local.rsplit(':').next()?.trim_end_matches(']');
    let port: u32 = port_str.parse().ok().filter(|&p| p > 0)?;

    let users_field = fields.iter().find(|f| f.starts_with("users:"));
    let (process, pid) = match users_field {
        Some(s) => parse_users(s),
        None => ("?".to_string(), None),
    };

    let cwd = pid.map(read_cwd).unwrap_or_else(|| "-".to_string());
    let cmdline = pid.map(read_cmdline).unwrap_or_else(|| "-".to_string());
    let docker = docker_map.get(&port).cloned();

    Some(Entry {
        proto,
        port,
        pid,
        process,
        cwd,
        cmdline,
        docker,
        stats: Stats::default(),
    })
}

fn parse_users(s: &str) -> (String, Option<u32>) {
    let name = extract_between(s, '"', '"').unwrap_or_else(|| "?".to_string());
    let pid = extract_pid(s);
    (name, pid)
}

fn extract_between(s: &str, open: char, close: char) -> Option<String> {
    let start = s.find(open)? + 1;
    let rest = &s[start..];
    let end = rest.find(close)?;
    Some(rest[..end].to_string())
}

fn extract_pid(s: &str) -> Option<u32> {
    let idx = s.find("pid=")? + 4;
    let rest = &s[idx..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn read_cwd(pid: u32) -> String {
    fs::read_link(format!("/proc/{}/cwd", pid))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "?".to_string())
}

fn read_cmdline(pid: u32) -> String {
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
    let output = match Command::new("ps")
        .args(["-o", "pid=,pcpu=,rss=,nlwp=,etime=,user=", "-p", &pid_arg])
        .output()
    {
        Ok(o) => o,
        _ => return,
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut map: HashMap<u32, Stats> = HashMap::new();
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 6 {
            continue;
        }
        let Ok(pid) = parts[0].parse::<u32>() else {
            continue;
        };
        let cpu = format!("{}%", parts[1]);
        let rss_kb: u64 = parts[2].parse().unwrap_or(0);
        let mem = format_mem(rss_kb * 1024);
        let threads = parts[3].parse::<u32>().ok();
        let uptime = format_etime(parts[4]);
        let user = Some(parts[5].to_string());
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
    let mut cmd = Command::new("docker");
    cmd.args([
        "stats",
        "--no-stream",
        "--format",
        "{{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}",
    ]);
    for n in &names {
        cmd.arg(n);
    }
    let output = match cmd.output() {
        Ok(o) if o.status.success() => o,
        _ => return,
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

fn nz(s: &str) -> String {
    if s.is_empty() {
        "-".to_string()
    } else {
        s.to_string()
    }
}

fn print_info(entries: &[Entry]) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    if entries.is_empty() {
        eprintln!("(no matching port found)");
        return;
    }

    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            outln!(out, "");
        }
        outln!(out, "─────────────────────────────────────────────");
        let rows: Vec<(&str, String)> = match &e.docker {
            Some(d) => docker_info_rows(e, d),
            None => local_info_rows(e),
        };
        let label_w = rows.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
        for (label, value) in &rows {
            outln!(out, "  {:<width$}  {}", label, value, width = label_w);
        }
    }
}

fn local_info_rows(e: &Entry) -> Vec<(&'static str, String)> {
    let mut rows: Vec<(&'static str, String)> = vec![
        ("PORT", format!("{}/{}", e.proto, e.port)),
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

    let rows: Vec<Vec<String>> = entries
        .iter()
        .map(|e| {
            let (process, job) = match &e.docker {
                Some(d) => ("docker".to_string(), d.name.clone()),
                None => (e.process.clone(), e.cwd.clone()),
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

    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }

    let fmt_row = |cells: &[String]| -> String {
        cells
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{:<width$}", c, width = *w))
            .collect::<Vec<_>>()
            .join("  ")
    };
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let header_cells: Vec<String> = headers.iter().map(|s| s.to_string()).collect();
    outln!(out, "{}", fmt_row(&header_cells));
    let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    outln!(out, "{}", sep.join("  "));
    for row in &rows {
        outln!(out, "{}", fmt_row(row));
    }

    if entries.is_empty() {
        if dev_mode {
            eprintln!("\n(no listening ports found — try running with sudo)");
        } else {
            eprintln!("\n(no user servers to display — run `lport --dev` to see everything)");
        }
    }
}
