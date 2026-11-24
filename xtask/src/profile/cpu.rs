use std::collections::BTreeMap;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Viewer,
    Multiplexer,
    Terminal,
    Display,
}

impl Role {
    pub const ALL: [Role; 4] = [
        Role::Viewer,
        Role::Multiplexer,
        Role::Terminal,
        Role::Display,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Multiplexer => "multiplexer",
            Role::Terminal => "terminal",
            Role::Display => "display",
        }
    }

    pub fn of(name: &str, command_line: &str) -> Option<Role> {
        let name = name.rsplit('/').next().unwrap_or(name);
        let lower = name.to_ascii_lowercase();
        if lower == "termleaf"
            || lower == "pixel"
            || lower.starts_with("pixel helper")
            || command_line.contains("@zenbu-labs/pixel")
        {
            return Some(Role::Viewer);
        }
        if lower == "herdr" || lower.starts_with("tmux") || lower.starts_with("zellij") {
            return Some(Role::Multiplexer);
        }
        if ["ghostty", "kitty", "wezterm-gui", "xterm", "rex"]
            .iter()
            .any(|terminal| lower == *terminal)
        {
            return Some(Role::Terminal);
        }
        if lower == "xvfb" {
            return Some(Role::Display);
        }
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    pub role: Role,
    pub micros: u64,
}

pub type Snapshot = Vec<Process>;

pub fn spent(before: &[Process], after: &[Process]) -> BTreeMap<Role, u64> {
    let earlier: BTreeMap<u32, u64> = before
        .iter()
        .map(|process| (process.pid, process.micros))
        .collect();
    let mut spent = BTreeMap::new();
    for process in after {
        let start = earlier.get(&process.pid).copied().unwrap_or(0);
        *spent.entry(process.role).or_insert(0) += process.micros.saturating_sub(start);
    }
    spent
}

pub fn snapshot() -> Snapshot {
    platform_snapshot()
}

#[cfg(target_os = "linux")]
fn platform_snapshot() -> Snapshot {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let ticks = clock_ticks();
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|pid| {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let (name, ticks_used) = parse_stat(&stat)?;
            let command_line = std::fs::read(format!("/proc/{pid}/cmdline"))
                .map(|bytes| String::from_utf8_lossy(&bytes).replace('\0', " "))
                .unwrap_or_default();
            let role = Role::of(&name, &command_line)?;
            Some(Process {
                pid,
                role,
                micros: ticks_used * 1_000_000 / ticks,
            })
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn clock_ticks() -> u64 {
    // SAFETY: sysconf only reads a configuration value.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    u64::try_from(ticks)
        .ok()
        .filter(|&ticks| ticks > 0)
        .unwrap_or(100)
}

#[cfg(not(target_os = "linux"))]
fn platform_snapshot() -> Snapshot {
    let Ok(output) = Command::new("ps")
        .args(["-A", "-o", "pid=,time=,comm="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_ps_line)
        .collect()
}

#[cfg(target_os = "linux")]
pub fn parse_stat(stat: &str) -> Option<(String, u64)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let name = stat.get(open + 1..close)?.to_owned();
    let fields: Vec<&str> = stat.get(close + 2..)?.split_whitespace().collect();
    let user: u64 = fields.get(11)?.parse().ok()?;
    let system: u64 = fields.get(12)?.parse().ok()?;
    Some((name, user + system))
}

#[cfg(not(target_os = "linux"))]
pub fn parse_ps_line(line: &str) -> Option<Process> {
    let mut fields = line.split_whitespace();
    let pid = fields.next()?.parse().ok()?;
    let micros = parse_ps_time(fields.next()?)?;
    let command = fields.collect::<Vec<_>>().join(" ");
    let role = Role::of(&command, &command)?;
    Some(Process { pid, role, micros })
}

#[cfg(not(target_os = "linux"))]
pub fn parse_ps_time(time: &str) -> Option<u64> {
    let (clock, fraction) = time.split_once('.').unwrap_or((time, "0"));
    let seconds = clock.split(':').try_fold(0_u64, |total, part| {
        Some(total * 60 + part.parse::<u64>().ok()?)
    })?;
    let digits = fraction.len().min(6);
    let fraction: u64 = fraction.get(..digits)?.parse().ok()?;
    let scale = 10_u64.pow(6 - u32::try_from(digits).ok()?);
    Some(seconds * 1_000_000 + fraction * scale)
}

pub fn describe() -> String {
    Command::new("uname")
        .arg("-srm")
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewers_multiplexers_and_terminals_are_told_apart() {
        assert_eq!(Role::of("termleaf", ""), Some(Role::Viewer));
        assert_eq!(Role::of("pixel", ""), Some(Role::Viewer));
        assert_eq!(
            Role::of(
                "node",
                "node /w/node_modules/@zenbu-labs/pixel/dist/bin.js ."
            ),
            Some(Role::Viewer)
        );
        assert_eq!(Role::of("tmux: server", ""), Some(Role::Multiplexer));
        assert_eq!(Role::of("herdr", ""), Some(Role::Multiplexer));
        assert_eq!(Role::of("ghostty", ""), Some(Role::Terminal));
        assert_eq!(
            Role::of("/Applications/Rex.app/Contents/MacOS/Rex", ""),
            Some(Role::Terminal)
        );
        assert_eq!(Role::of("Xvfb", ""), Some(Role::Display));
        assert_eq!(Role::of("bash", ""), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_stat_line_gives_the_name_and_cpu_ticks() {
        let stat =
            "4242 (tmux: server) S 1 4242 4242 0 -1 4194560 1 0 0 0 30 12 0 0 20 0 1 0 1 1 1";
        assert_eq!(parse_stat(stat), Some(("tmux: server".to_owned(), 42)));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn ps_times_are_read_with_their_fraction() {
        assert_eq!(parse_ps_time("0:01.23"), Some(1_230_000));
        assert_eq!(parse_ps_time("1:02:03.5"), Some(3_723_500_000));
    }

    #[test]
    fn cpu_spent_is_the_growth_of_each_process_by_role() {
        let process = |pid, role, micros| Process { pid, role, micros };
        let before = vec![
            process(1, Role::Viewer, 100),
            process(2, Role::Terminal, 50),
        ];
        let after = vec![
            process(1, Role::Viewer, 400),
            process(2, Role::Terminal, 50),
            process(3, Role::Viewer, 25),
        ];
        let spent = spent(&before, &after);
        assert_eq!(spent.get(&Role::Viewer), Some(&325));
        assert_eq!(spent.get(&Role::Terminal), Some(&0));
    }
}
