use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::process::{Identity, Pid, Process, ProcessTable, StartTime, program_name};

const PROC: &str = "/proc";
const ACCEPTING_CONNECTIONS: u32 = 0x0001_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Device {
    major: u64,
    minor: u64,
}

impl Device {
    fn from_tty_nr(tty_nr: u64) -> Self {
        Self {
            major: (tty_nr >> 8) & 0xfff,
            minor: (tty_nr & 0xff) | ((tty_nr >> 12) & 0xf_ff00),
        }
    }

    fn from_rdev(rdev: u64) -> Self {
        Self {
            major: ((rdev >> 8) & 0xfff) | ((rdev >> 32) & !0xfff),
            minor: (rdev & 0xff) | ((rdev >> 12) & !0xff),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Stat {
    pid: Pid,
    command: String,
    parent: Pid,
    group: Pid,
    tty: Option<Device>,
    foreground_group: Option<Pid>,
    start: StartTime,
}

fn parse_stat(text: &str) -> Option<Stat> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let pid = text.get(..open)?.trim().parse().ok()?;
    let command = text.get(open + 1..close)?.to_owned();
    let fields: Vec<&str> = text.get(close + 1..)?.split_whitespace().collect();
    let field = |number: usize| fields.get(number - 3).copied();
    let pid_field = |number: usize| field(number)?.parse::<i64>().ok();
    let positive = |value: i64| u32::try_from(value).ok().filter(|pid| *pid > 0).map(Pid);
    let tty_nr: u64 = field(7)?.parse().ok()?;
    Some(Stat {
        pid: Pid(pid),
        command,
        parent: Pid(u32::try_from(pid_field(4)?).unwrap_or_default()),
        group: positive(pid_field(5)?)?,
        tty: (tty_nr != 0).then(|| Device::from_tty_nr(tty_nr)),
        foreground_group: positive(pid_field(8)?),
        start: StartTime(field(22)?.parse().ok()?),
    })
}

fn parse_uid(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn parse_listening_unix_sockets(table: &str) -> HashMap<u64, PathBuf> {
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = Vec::with_capacity(7);
            let mut rest = line;
            while fields.len() < 7 {
                let (field, after) = next_field(rest)?;
                fields.push(field);
                rest = after;
            }
            let flags = u32::from_str_radix(fields[3], 16).ok()?;
            let inode = fields[6].parse().ok()?;
            let path = rest.trim_start_matches(' ');
            (path.starts_with('/') && flags & ACCEPTING_CONNECTIONS != 0)
                .then(|| (inode, PathBuf::from(path)))
        })
        .collect()
}

fn next_field(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_start_matches(' ');
    if text.is_empty() {
        return None;
    }
    Some(text.split_once(' ').unwrap_or((text, "")))
}

fn socket_inode(link: &Path) -> Option<u64> {
    link.to_str()?
        .strip_prefix("socket:[")?
        .strip_suffix(']')?
        .parse()
        .ok()
}

fn parse_arguments(cmdline: &[u8]) -> Vec<String> {
    cmdline
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .map(|argument| String::from_utf8_lossy(argument).into_owned())
        .collect()
}

#[derive(Debug, Clone)]
pub struct Procfs {
    root: PathBuf,
}

impl Default for Procfs {
    fn default() -> Self {
        Self {
            root: PathBuf::from(PROC),
        }
    }
}

impl Procfs {
    fn entry(&self, pid: Pid) -> PathBuf {
        self.root.join(pid.0.to_string())
    }

    fn stat(&self, pid: Pid) -> Option<Stat> {
        parse_stat(&fs::read_to_string(self.entry(pid).join("stat")).ok()?)
    }

    fn pids(&self) -> Vec<Pid> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse().ok().map(Pid))
            .collect()
    }

    fn stats(&self) -> impl Iterator<Item = Stat> + '_ {
        self.pids().into_iter().filter_map(|pid| self.stat(pid))
    }

    fn complete(&self, stat: Stat) -> Option<Process> {
        let entry = self.entry(stat.pid);
        let uid = parse_uid(&fs::read_to_string(entry.join("status")).ok()?)?;
        let program = fs::read_link(entry.join("exe"))
            .map_or(stat.command, |exe| program_name(&exe.to_string_lossy()));
        Some(Process {
            identity: Identity {
                pid: stat.pid,
                start: stat.start,
            },
            parent: stat.parent,
            group: stat.group,
            foreground_group: stat.foreground_group,
            uid,
            program,
        })
    }
}

impl ProcessTable for Procfs {
    fn process(&self, pid: Pid) -> Option<Process> {
        self.complete(self.stat(pid)?)
    }

    fn children(&self, pid: Pid) -> Vec<Process> {
        self.stats()
            .filter(|stat| stat.parent == pid)
            .filter_map(|stat| self.complete(stat))
            .collect()
    }

    fn on_tty(&self, tty: &Path) -> Option<Process> {
        let device = Device::from_rdev(fs::metadata(tty).ok()?.rdev());
        let stat = self.stats().find(|stat| stat.tty == Some(device))?;
        self.complete(stat)
    }

    fn arguments(&self, pid: Pid) -> Vec<String> {
        fs::read(self.entry(pid).join("cmdline"))
            .map(|cmdline| parse_arguments(&cmdline))
            .unwrap_or_default()
    }

    fn cwd(&self, pid: Pid) -> Option<PathBuf> {
        fs::read_link(self.entry(pid).join("cwd")).ok()
    }

    fn listening_sockets(&self, pid: Pid) -> Vec<PathBuf> {
        let Ok(fds) = fs::read_dir(self.entry(pid).join("fd")) else {
            return Vec::new();
        };
        let inodes: Vec<u64> = fds
            .flatten()
            .filter_map(|fd| socket_inode(&fs::read_link(fd.path()).ok()?))
            .collect();
        if inodes.is_empty() {
            return Vec::new();
        }
        let table = fs::read_to_string(self.root.join("net/unix")).unwrap_or_default();
        let mut listening = parse_listening_unix_sockets(&table);
        inodes
            .iter()
            .filter_map(|inode| listening.remove(inode))
            .collect()
    }

    fn own_uid(&self) -> Option<u32> {
        parse_uid(&fs::read_to_string(self.root.join("self/status")).ok()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NVIM_STAT: &str = "378223 (nvim) S 378210 378223 378210 34818 378223 4194560 537 0 50 0 0 0 0 0 20 0 1 0 4604758 11960320 2111 18446744073709551615";
    const SHELL_STAT: &str = "378210 (bash) S 378200 378210 378210 34818 378223 4194560 488 413 0 0 0 0 0 0 20 0 1 0 4604757 4866048 994 18446744073709551615";

    #[test]
    fn stat_gives_the_foreground_group_of_the_tty_and_the_start_time() {
        let shell = parse_stat(SHELL_STAT).expect("a stat line");
        assert_eq!(shell.pid, Pid(378_210));
        assert_eq!(shell.command, "bash");
        assert_eq!(shell.parent, Pid(378_200));
        assert_eq!(shell.group, Pid(378_210));
        assert_eq!(shell.foreground_group, Some(Pid(378_223)));
        assert_eq!(shell.start, StartTime(4_604_757));
        let editor = parse_stat(NVIM_STAT).expect("a stat line");
        assert_eq!(editor.group, editor.pid);
        assert_eq!(editor.tty, shell.tty);
    }

    #[test]
    fn a_command_name_with_spaces_and_parentheses_is_read_whole() {
        let stat = parse_stat(
            "42 (my (odd) name) S 1 42 42 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 1 0 777 0 0 0",
        )
        .expect("a stat line");
        assert_eq!(stat.command, "my (odd) name");
        assert_eq!(stat.tty, None);
        assert_eq!(stat.foreground_group, None);
        assert_eq!(stat.start, StartTime(777));
    }

    #[test]
    fn a_cut_short_stat_line_is_refused() {
        assert_eq!(parse_stat("42 (vim) S 1 42 42 34818"), None);
        assert_eq!(parse_stat("not a stat line"), None);
    }

    #[test]
    fn tty_numbers_from_stat_match_device_numbers_from_the_tty_file() {
        let pts_2 = 34_818;
        assert_eq!(
            Device::from_tty_nr(pts_2),
            Device {
                major: 136,
                minor: 2
            }
        );
        assert_eq!(Device::from_rdev(0x8802), Device::from_tty_nr(pts_2));
        let pts_300 = (136 << 8) | (300 & 0xff) | ((300 & !0xff) << 12);
        assert_eq!(
            Device::from_tty_nr(pts_300),
            Device {
                major: 136,
                minor: 300
            }
        );
    }

    #[test]
    fn the_real_uid_is_read_from_status() {
        assert_eq!(
            parse_uid("Name:\tnvim\nUid:\t1000\t1000\t1000\t1000\nGid:\t1000\n"),
            Some(1000)
        );
        assert_eq!(parse_uid("Name:\tnvim\n"), None);
    }

    #[test]
    fn only_listening_unix_sockets_with_a_path_are_kept() {
        let table = "Num       RefCount Protocol Flags    Type St Inode Path\n\
            000000002281ad72: 00000003 00000000 00000000 0001 03 480042\n\
            000000004de22557: 00000002 00000000 00010000 0001 01 2886538 /tmp/nvim.box/9yDIj6/nvim.378235.0\n\
            000000004de22558: 00000002 00000000 00000000 0001 03 2886539 /tmp/nvim.box/9yDIj6/nvim.378235.0\n\
            000000004de22559: 00000002 00000000 00010000 0001 01 2886540 @abstract\n";
        let listening = parse_listening_unix_sockets(table);
        assert_eq!(listening.len(), 1);
        assert_eq!(
            listening.get(&2_886_538),
            Some(&PathBuf::from("/tmp/nvim.box/9yDIj6/nvim.378235.0"))
        );
    }

    #[test]
    fn a_socket_path_with_spaces_is_kept_whole() {
        let table = "Num       RefCount Protocol Flags    Type St Inode Path\n\
            000000004de22557: 00000002 00000000 00010000 0001 01  2886538 /tmp/my thesis/nvim  pipe\n";
        assert_eq!(
            parse_listening_unix_sockets(table).get(&2_886_538),
            Some(&PathBuf::from("/tmp/my thesis/nvim  pipe"))
        );
    }

    #[test]
    fn socket_links_name_their_inode() {
        assert_eq!(socket_inode(Path::new("socket:[2886538]")), Some(2_886_538));
        assert_eq!(socket_inode(Path::new("/dev/pts/2")), None);
        assert_eq!(socket_inode(Path::new("pipe:[12]")), None);
    }

    #[test]
    fn arguments_are_split_at_nul_bytes() {
        assert_eq!(
            parse_arguments(b"hx\0ch5.tex:77\0notes.md\0"),
            ["hx", "ch5.tex:77", "notes.md"]
        );
    }

    #[test]
    fn our_own_process_is_read_back_from_proc() {
        let procfs = Procfs::default();
        let pid = Pid(std::process::id());
        let us = procfs.process(pid).expect("our own process");
        assert_eq!(Some(us.uid), procfs.own_uid());
        assert_eq!(us.pid(), pid);
        assert_eq!(procfs.cwd(pid), std::env::current_dir().ok());
        assert!(!procfs.arguments(pid).is_empty());
        assert!(
            procfs
                .children(us.parent)
                .iter()
                .any(|child| child.pid() == pid)
        );
        assert!(procfs.listening_sockets(pid).is_empty());
    }
}
