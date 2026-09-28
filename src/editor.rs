mod detect;
mod evidence;
mod inject;
#[cfg(test)]
mod live;
mod probe;
mod process;
#[cfg(target_os = "linux")]
mod procfs;
mod ps;
mod safety;
mod tmux;

use process::ProcessTable;

#[cfg(target_os = "linux")]
fn system_processes() -> anyhow::Result<impl ProcessTable> {
    Ok(procfs::Procfs::default())
}

#[cfg(not(target_os = "linux"))]
fn system_processes() -> anyhow::Result<impl ProcessTable> {
    ps::Ps::read()
}

#[cfg(test)]
mod tests {
    use super::process::Pid;
    use super::*;

    #[test]
    fn the_system_process_table_knows_our_own_process() {
        let table = system_processes().expect("the process table");
        let us = table
            .process(Pid(std::process::id()))
            .expect("our own process");
        assert_eq!(table.own_uid(), Some(us.uid));
    }
}
