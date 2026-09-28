use std::path::PathBuf;

use super::process::{Pid, ProcessTable};

pub fn neovim_socket(table: &impl ProcessTable, pid: Pid) -> Option<PathBuf> {
    let sockets: Vec<PathBuf> = std::iter::once(pid)
        .chain(table.children(pid).iter().map(|child| child.pid()))
        .flat_map(|owner| table.listening_sockets(owner))
        .collect();
    let named_by_neovim = sockets.iter().find(|socket| {
        socket
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("nvim."))
    });
    named_by_neovim.or(sockets.first()).cloned()
}

#[cfg(test)]
mod tests {
    use super::super::process::fake::FakeTable;
    use super::*;

    #[test]
    fn a_neovim_socket_is_found_on_its_embedded_child() {
        let mut table = FakeTable::default();
        table
            .spawn(40, 30, "nvim")
            .spawn(41, 40, "nvim")
            .listens(41, "/run/user/1000/lsp.sock")
            .listens(41, "/run/user/1000/nvim.41.0");
        assert_eq!(
            neovim_socket(&table, Pid(40)),
            Some(PathBuf::from("/run/user/1000/nvim.41.0"))
        );
    }

    #[test]
    fn a_custom_listen_socket_is_taken_when_it_is_the_only_one() {
        let mut table = FakeTable::default();
        table.spawn(40, 30, "nvim").listens(40, "/tmp/thesis.pipe");
        assert_eq!(
            neovim_socket(&table, Pid(40)),
            Some(PathBuf::from("/tmp/thesis.pipe"))
        );
        table.spawn(50, 30, "nvim");
        assert_eq!(neovim_socket(&table, Pid(50)), None);
    }
}
