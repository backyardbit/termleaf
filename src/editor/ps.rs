use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::process::{Identity, Pid, Process, ProcessTable, StartTime, program_name};

const COLUMNS: &str = "pid=,ppid=,pgid=,tpgid=,uid=,tty=,lstart=,comm=";
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const SECONDS_PER_DAY: i64 = 86_400;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    process: Process,
    tty: Option<PathBuf>,
}

fn split_off(text: &str, count: usize) -> Option<(Vec<&str>, &str)> {
    let mut rest = text.trim_start();
    let mut tokens = Vec::with_capacity(count);
    for _ in 0..count {
        let end = rest.find(char::is_whitespace)?;
        tokens.push(rest.get(..end)?);
        rest = rest.get(end..)?.trim_start();
    }
    Some((tokens, rest))
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn parse_started(tokens: &[&str]) -> Option<StartTime> {
    let [_weekday, month, day, clock, year] = tokens else {
        return None;
    };
    let month = MONTHS.iter().position(|name| name == month)?;
    let month = i64::try_from(month).ok()? + 1;
    let mut clock = clock.split(':').map(str::parse::<i64>);
    let (Some(Ok(hours)), Some(Ok(minutes)), Some(Ok(seconds))) =
        (clock.next(), clock.next(), clock.next())
    else {
        return None;
    };
    let days = days_from_civil(year.parse().ok()?, month, day.parse().ok()?);
    let since_epoch = days * SECONDS_PER_DAY + hours * 3600 + minutes * 60 + seconds;
    u64::try_from(since_epoch).ok().map(StartTime)
}

fn parse_entry(line: &str) -> Option<Entry> {
    let (tokens, command) = split_off(line, 11)?;
    let number = |index: usize| tokens.get(index)?.parse::<i64>().ok();
    let pid = |index: usize| {
        number(index)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|pid| *pid > 0)
            .map(Pid)
    };
    let tty = tokens
        .get(5)
        .filter(|tty| !tty.starts_with('?'))
        .map(|tty| Path::new("/dev").join(tty));
    Some(Entry {
        process: Process {
            identity: Identity {
                pid: pid(0)?,
                start: parse_started(tokens.get(6..11)?)?,
            },
            parent: pid(1).unwrap_or(Pid(0)),
            group: pid(2)?,
            foreground_group: pid(3),
            uid: u32::try_from(number(4)?).ok()?,
            program: program_name(command.trim_end()),
        },
        tty,
    })
}

fn nvim_sockets(runtime: Option<&Path>, temporary: &Path, user: &str, pid: Pid) -> Vec<PathBuf> {
    let name = format!("nvim.{}.0", pid.0);
    let in_runtime = runtime.map(|directory| directory.join(&name));
    let in_temporary = fs::read_dir(temporary.join(format!("nvim.{user}")))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path().join(&name));
    in_runtime
        .into_iter()
        .chain(in_temporary)
        .filter(|socket| socket.exists())
        .collect()
}

#[cfg(target_os = "macos")]
fn working_directory(pid: Pid) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let pid = libc::c_int::try_from(pid.0).ok()?;
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_vnodepathinfo>()).ok()?;
    // SAFETY: proc_vnodepathinfo is plain C data made of integers and character arrays, for which all zero bytes are a valid value.
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    // SAFETY: the buffer is a live proc_vnodepathinfo and size is its exact size, which is what PROC_PIDVNODEPATHINFO fills in.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            std::ptr::from_mut(&mut info).cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    let path: Vec<u8> = info
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .map(|character| character.to_ne_bytes()[0])
        .take_while(|byte| *byte != 0)
        .collect();
    (!path.is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(&path)))
}

#[cfg(not(target_os = "macos"))]
fn working_directory(_pid: Pid) -> Option<PathBuf> {
    None
}

#[derive(Debug, Clone, Default)]
pub struct Ps {
    entries: Vec<Entry>,
}

impl Ps {
    pub fn read() -> Result<Self> {
        let output = Command::new("ps")
            .env("LC_ALL", "C")
            .args(["-A", "-o", COLUMNS])
            .output()
            .context("could not run ps")?;
        if !output.status.success() {
            bail!("ps failed");
        }
        Ok(Self::parse(&String::from_utf8_lossy(&output.stdout)))
    }

    fn parse(listing: &str) -> Self {
        Self {
            entries: listing.lines().filter_map(parse_entry).collect(),
        }
    }
}

impl ProcessTable for Ps {
    fn process(&self, pid: Pid) -> Option<Process> {
        self.entries
            .iter()
            .find(|entry| entry.process.pid() == pid)
            .map(|entry| entry.process.clone())
    }

    fn children(&self, pid: Pid) -> Vec<Process> {
        self.entries
            .iter()
            .filter(|entry| entry.process.parent == pid)
            .map(|entry| entry.process.clone())
            .collect()
    }

    fn on_tty(&self, tty: &Path) -> Option<Process> {
        self.entries
            .iter()
            .find(|entry| entry.tty.as_deref() == Some(tty))
            .map(|entry| entry.process.clone())
    }

    fn arguments(&self, pid: Pid) -> Vec<String> {
        Command::new("ps")
            .env("LC_ALL", "C")
            .args(["-o", "args=", "-p", &pid.0.to_string()])
            .output()
            .map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn cwd(&self, pid: Pid) -> Option<PathBuf> {
        working_directory(pid)
    }

    fn listening_sockets(&self, pid: Pid) -> Vec<PathBuf> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
        let user = std::env::var("USER").unwrap_or_default();
        nvim_sockets(runtime.as_deref(), &std::env::temp_dir(), &user, pid)
    }

    fn own_uid(&self) -> Option<u32> {
        self.process(Pid(std::process::id()))
            .map(|process| process.uid)
    }

    fn processes(&self) -> Vec<Process> {
        self.entries
            .iter()
            .map(|entry| entry.process.clone())
            .collect()
    }

    fn environment(&self, _pid: Pid) -> Vec<(String, String)> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINUX: &str = "   378210  378200  378210  378223  1000 pts/2    Sun Sep 27 23:49:10 2026 bash\n   378223  378210  378223  378223  1000 pts/2    Sun Sep 27 23:49:11 2026 nvim\n   378235  378223  378235      -1  1000 ?        Sun Sep 27 23:49:11 2026 nvim\n";

    #[test]
    fn a_linux_listing_gives_the_foreground_group_of_each_tty() {
        let ps = Ps::parse(LINUX);
        let shell = ps.process(Pid(378_210)).expect("the shell");
        assert_eq!(shell.foreground_group, Some(Pid(378_223)));
        assert_eq!(shell.program, "bash");
        assert_eq!(
            ps.on_tty(Path::new("/dev/pts/2"))
                .map(|process| process.pid()),
            Some(Pid(378_210))
        );
        assert_eq!(
            ps.process(Pid(378_235)).map(|embed| embed.foreground_group),
            Some(None)
        );
        assert_eq!(ps.children(Pid(378_223)).len(), 1);
        assert_eq!(
            ps.foreground_leaders()
                .iter()
                .map(Process::pid)
                .collect::<Vec<_>>(),
            [Pid(378_223)]
        );
    }

    #[test]
    fn start_times_count_seconds_from_the_epoch() {
        assert_eq!(
            parse_started(&["Thu", "Jan", "1", "00:00:00", "1970"]),
            Some(StartTime(0))
        );
        assert_eq!(
            parse_started(&["Sun", "Sep", "27", "23:49:10", "2026"]),
            Some(StartTime(1_790_552_950))
        );
        assert_eq!(
            parse_started(&["Sun", "Sept", "27", "23:49:10", "2026"]),
            None
        );
    }

    #[test]
    fn a_line_cut_short_is_skipped() {
        assert!(
            Ps::parse("  812   811   812   840   501 ttys003  Mon Sep")
                .entries
                .is_empty()
        );
    }

    #[test]
    fn nvim_sockets_are_found_under_the_runtime_and_temporary_directories() {
        let root = std::env::temp_dir().join(format!("termleaf-ps-{}", std::process::id()));
        let runtime = root.join("run");
        let session = root.join("tmp/nvim.ada/Xy12");
        fs::create_dir_all(&runtime).expect("a runtime directory");
        fs::create_dir_all(&session).expect("a session directory");
        fs::write(runtime.join("nvim.77.0"), "").expect("a runtime socket");
        fs::write(session.join("nvim.77.0"), "").expect("a temporary socket");
        fs::write(session.join("nvim.78.0"), "").expect("another nvim's socket");
        assert_eq!(
            nvim_sockets(Some(&runtime), &root.join("tmp"), "ada", Pid(77)),
            [runtime.join("nvim.77.0"), session.join("nvim.77.0")]
        );
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ps_knows_our_own_working_directory() {
        let ps = Ps::read().expect("ps runs");
        assert_eq!(
            ps.cwd(Pid(std::process::id())),
            std::env::current_dir().ok()
        );
    }

    #[test]
    fn ps_agrees_with_the_system_about_our_own_process() {
        let ps = Ps::read().expect("ps runs");
        let pid = Pid(std::process::id());
        let us = ps.process(pid).expect("our own process");
        assert_eq!(ps.own_uid(), Some(us.uid));
        assert!(
            ps.children(us.parent)
                .iter()
                .any(|child| child.pid() == pid)
        );
        assert!(!ps.arguments(pid).is_empty());
        assert!(ps.listening_sockets(pid).is_empty());
        assert!(ps.environment(pid).is_empty());
    }
}
