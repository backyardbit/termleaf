use std::collections::HashMap;
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
    fn processes(&self) -> Vec<Process>;
    fn environment(&self, pid: Pid) -> Vec<(String, String)>;

    fn foreground_leaders(&self) -> Vec<Process> {
        self.processes()
            .into_iter()
            .filter(|process| process.foreground_group == Some(process.pid()))
            .collect()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PaneTags<'a> {
    pub session_variable: &'a str,
    pub session: &'a str,
    pub pane_variable: &'a str,
    pub servers: &'a [&'a str],
}

pub fn pane_roots(table: &impl ProcessTable, tags: PaneTags<'_>) -> HashMap<String, Pid> {
    let mut members: HashMap<String, Vec<Process>> = HashMap::new();
    for process in table.processes() {
        let environment = table.environment(process.pid());
        let value = |name: &str| {
            environment
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        if value(tags.session_variable) != Some(tags.session) {
            continue;
        }
        if let Some(pane) = value(tags.pane_variable) {
            members.entry(pane.to_owned()).or_default().push(process);
        }
    }
    members
        .into_iter()
        .filter_map(|(pane, processes)| {
            let spawned_by_server = processes
                .iter()
                .filter(|process| {
                    table
                        .process(process.parent)
                        .is_some_and(|parent| tags.servers.contains(&parent.program.as_str()))
                })
                .min_by_key(|process| process.identity.start.0);
            let oldest = || {
                processes
                    .iter()
                    .min_by_key(|process| process.identity.start.0)
            };
            spawned_by_server
                .or_else(oldest)
                .map(|root| (pane, root.pid()))
        })
        .collect()
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
        environments: HashMap<Pid, Vec<(String, String)>>,
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

        pub fn foreground(&mut self, pid: u32, group: u32) -> &mut Self {
            self.edit(pid, |process| process.foreground_group = Some(Pid(group)))
        }

        pub fn owned_by(&mut self, pid: u32, uid: u32) -> &mut Self {
            self.edit(pid, |process| process.uid = uid)
        }

        pub fn started(&mut self, pid: u32, start: u64) -> &mut Self {
            self.edit(pid, |process| process.identity.start = StartTime(start))
        }

        pub fn exit(&mut self, pid: u32) -> &mut Self {
            self.processes.retain(|process| process.pid() != Pid(pid));
            self
        }

        pub fn tty(&mut self, tty: &str, pid: u32) -> &mut Self {
            self.ttys.insert(PathBuf::from(tty), Pid(pid));
            self
        }

        pub fn argv(&mut self, pid: u32, arguments: &[&str], cwd: &str) -> &mut Self {
            self.arguments.insert(
                Pid(pid),
                arguments
                    .iter()
                    .map(|argument| (*argument).to_owned())
                    .collect(),
            );
            self.cwds.insert(Pid(pid), PathBuf::from(cwd));
            self
        }

        pub fn variable(&mut self, pid: u32, name: &str, value: &str) -> &mut Self {
            self.environments
                .entry(Pid(pid))
                .or_default()
                .push((name.to_owned(), value.to_owned()));
            self
        }

        pub fn listens(&mut self, pid: u32, socket: &str) -> &mut Self {
            self.sockets
                .entry(Pid(pid))
                .or_default()
                .push(PathBuf::from(socket));
            self
        }

        fn edit(&mut self, pid: u32, change: impl FnOnce(&mut Process)) -> &mut Self {
            if let Some(process) = self
                .processes
                .iter_mut()
                .find(|process| process.pid() == Pid(pid))
            {
                change(process);
            }
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

        fn processes(&self) -> Vec<Process> {
            self.processes.clone()
        }

        fn environment(&self, pid: Pid) -> Vec<(String, String)> {
            self.environments.get(&pid).cloned().unwrap_or_default()
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

    const ZELLIJ: PaneTags<'static> = PaneTags {
        session_variable: "ZELLIJ_SESSION_NAME",
        session: "thesis",
        pane_variable: "ZELLIJ_PANE_ID",
        servers: &["zellij"],
    };

    fn in_pane(table: &mut FakeTable, pid: u32, pane: &str) {
        table
            .variable(pid, "ZELLIJ_SESSION_NAME", "thesis")
            .variable(pid, "ZELLIJ_PANE_ID", pane);
    }

    #[test]
    fn each_pane_is_rooted_at_the_process_the_server_spawned() {
        let mut table = FakeTable::default();
        table
            .spawn(10, 1, "zellij")
            .spawn(15, 1, "tmux")
            .spawn(20, 10, "bash")
            .spawn(21, 20, "tmux")
            .spawn(40, 15, "bash")
            .started(40, 5)
            .spawn(30, 10, "hx")
            .spawn(50, 10, "bash")
            .spawn(60, 10, "bash");
        for (pid, pane) in [(20, "1"), (21, "1"), (40, "1"), (30, "2")] {
            in_pane(&mut table, pid, pane);
        }
        table
            .variable(50, "ZELLIJ_SESSION_NAME", "other")
            .variable(50, "ZELLIJ_PANE_ID", "3")
            .variable(60, "ZELLIJ_SESSION_NAME", "thesis");
        let roots = pane_roots(&table, ZELLIJ);
        assert_eq!(roots.len(), 2);
        assert_eq!(roots.get("1"), Some(&Pid(20)));
        assert_eq!(roots.get("2"), Some(&Pid(30)));
    }

    #[test]
    fn without_the_server_the_oldest_process_of_a_pane_is_its_root() {
        let mut table = FakeTable::default();
        table
            .spawn(50, 1, "bash")
            .spawn(51, 50, "vim")
            .started(51, 1);
        in_pane(&mut table, 50, "3");
        in_pane(&mut table, 51, "3");
        assert_eq!(pane_roots(&table, ZELLIJ).get("3"), Some(&Pid(51)));
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
