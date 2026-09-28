use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Pid(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartTime(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    pub pid: Pid,
    pub start: StartTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    pub identity: Identity,
    pub parent: Pid,
    pub group: Pid,
    pub foreground_group: Option<Pid>,
    pub uid: u32,
    pub program: String,
}

impl Process {
    pub fn pid(&self) -> Pid {
        self.identity.pid
    }
}

pub trait ProcessTable {
    fn process(&self, pid: Pid) -> Option<Process>;
    fn children(&self, pid: Pid) -> Vec<Process>;
    fn on_tty(&self, tty: &Path) -> Option<Process>;
    fn arguments(&self, pid: Pid) -> Vec<String>;
    fn cwd(&self, pid: Pid) -> Option<PathBuf>;
    fn listening_sockets(&self, pid: Pid) -> Vec<PathBuf>;
    fn own_uid(&self) -> Option<u32>;
}

const SHELLS: &[&str] = &[
    "sh", "bash", "dash", "zsh", "fish", "ksh", "mksh", "tcsh", "csh", "nu", "elvish", "xonsh",
    "pwsh", "ion", "oil", "osh", "ysh",
];

pub fn is_shell(program: &str) -> bool {
    SHELLS.contains(&program.trim_start_matches('-'))
}

pub fn program_name(executable: &str) -> String {
    let path = executable.strip_suffix(" (deleted)").unwrap_or(executable);
    Path::new(path).file_name().map_or_else(
        || path.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

pub fn ancestors(table: &impl ProcessTable, pid: Pid) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = vec![pid];
    let mut next = table.process(pid).map(|process| process.parent);
    while let Some(parent) = next.filter(|parent| parent.0 > 1 && !seen.contains(parent)) {
        seen.push(parent);
        let Some(process) = table.process(parent) else {
            break;
        };
        names.push(process.program.clone());
        next = Some(process.parent);
    }
    names
}

#[cfg(test)]
pub mod fake {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use super::*;

    pub const OUR_UID: u32 = 1000;

    #[derive(Debug, Default)]
    pub struct FakeTable {
        processes: Vec<Process>,
        ttys: HashMap<PathBuf, Pid>,
        arguments: HashMap<Pid, Vec<String>>,
        cwds: HashMap<Pid, PathBuf>,
        sockets: HashMap<Pid, Vec<PathBuf>>,
    }

    impl FakeTable {
        pub fn spawn(&mut self, pid: u32, parent: u32, program: &str) -> &mut Self {
            self.processes.push(Process {
                identity: Identity {
                    pid: Pid(pid),
                    start: StartTime(u64::from(pid) * 10),
                },
                parent: Pid(parent),
                group: Pid(pid),
                foreground_group: None,
                uid: OUR_UID,
                program: program.to_owned(),
            });
            self
        }
    }

    impl ProcessTable for FakeTable {
        fn process(&self, pid: Pid) -> Option<Process> {
            self.processes
                .iter()
                .find(|process| process.pid() == pid)
                .cloned()
        }

        fn children(&self, pid: Pid) -> Vec<Process> {
            self.processes
                .iter()
                .filter(|process| process.parent == pid)
                .cloned()
                .collect()
        }

        fn on_tty(&self, tty: &Path) -> Option<Process> {
            self.ttys.get(tty).and_then(|pid| self.process(*pid))
        }

        fn arguments(&self, pid: Pid) -> Vec<String> {
            self.arguments.get(&pid).cloned().unwrap_or_default()
        }

        fn cwd(&self, pid: Pid) -> Option<PathBuf> {
            self.cwds.get(&pid).cloned()
        }

        fn listening_sockets(&self, pid: Pid) -> Vec<PathBuf> {
            self.sockets.get(&pid).cloned().unwrap_or_default()
        }

        fn own_uid(&self) -> Option<u32> {
            Some(OUR_UID)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeTable;
    use super::*;

    #[test]
    fn shells_are_known_by_name_and_as_login_shells() {
        assert!(is_shell("bash"));
        assert!(is_shell("-zsh"));
        assert!(is_shell("fish"));
        assert!(!is_shell("nvim"));
        assert!(!is_shell("less"));
    }

    #[test]
    fn a_program_is_named_after_its_executable() {
        assert_eq!(program_name("/usr/bin/vim.gtk3"), "vim.gtk3");
        assert_eq!(program_name("/opt/helix/hx"), "hx");
        assert_eq!(program_name("nvim"), "nvim");
    }

    #[test]
    fn an_executable_replaced_by_an_upgrade_keeps_its_name() {
        assert_eq!(program_name("/usr/bin/nvim (deleted)"), "nvim");
    }

    #[test]
    fn ancestors_list_the_parents_up_to_init() {
        let mut table = FakeTable::default();
        table
            .spawn(10, 1, "kitty")
            .spawn(20, 10, "bash")
            .spawn(30, 1, "tmux")
            .spawn(40, 30, "bash")
            .spawn(50, 40, "termleaf");
        assert_eq!(ancestors(&table, Pid(50)), ["bash", "tmux"]);
    }

    #[test]
    fn ancestors_stop_at_a_parent_that_is_gone_or_loops() {
        let mut table = FakeTable::default();
        table.spawn(50, 40, "termleaf");
        assert!(ancestors(&table, Pid(50)).is_empty());
        table.spawn(60, 70, "a").spawn(70, 60, "b");
        assert_eq!(ancestors(&table, Pid(60)), ["b"]);
    }
}
