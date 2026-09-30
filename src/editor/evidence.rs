use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use super::detect::Environment;
use super::probe::{Editor, EditorKind, LoadedFiles};
use super::process::{Pid, ProcessTable};

const SWAP_EXTENSIONS: [&str; 16] = [
    "swp", "swo", "swn", "swm", "swl", "swk", "swj", "swi", "swh", "swg", "swf", "swe", "swd",
    "swc", "swb", "swa",
];
const SWAP_MAGIC: &[u8] = b"b0VIM ";
const SWAP_PID_AT: usize = 24;
const SWAP_FILE_AT: usize = 108;
const SWAP_FILE_BYTES: usize = 900;
const SWAP_HEADER_BYTES: u64 = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapOwner {
    pub pid: Pid,
    pub file: PathBuf,
}

pub fn parse_swap_header(block: &[u8], home: Option<&Path>) -> Option<SwapOwner> {
    if !block.starts_with(SWAP_MAGIC) {
        return None;
    }
    let pid_bytes: [u8; 4] = block.get(SWAP_PID_AT..SWAP_PID_AT + 4)?.try_into().ok()?;
    let field = block.get(SWAP_FILE_AT..)?;
    let field = field.get(..SWAP_FILE_BYTES.min(field.len()))?;
    let name = field.split(|byte| *byte == 0).next()?;
    let name = std::str::from_utf8(name)
        .ok()
        .filter(|name| !name.is_empty())?;
    let file = match (name.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(name),
    };
    Some(SwapOwner {
        pid: Pid(u32::from_le_bytes(pid_bytes)),
        file,
    })
}

pub fn swap_candidates(file: &Path, state_home: Option<&Path>) -> Vec<PathBuf> {
    let Some(name) = file.file_name().map(|name| name.to_string_lossy()) else {
        return Vec::new();
    };
    let beside = file.parent().map(|directory| {
        SWAP_EXTENSIONS.map(|extension| directory.join(format!(".{name}.{extension}")))
    });
    let encoded = file.to_string_lossy().replace('/', "%");
    let neovim = state_home.map(|state| {
        SWAP_EXTENSIONS.map(|extension| {
            state
                .join("nvim/swap")
                .join(format!("{encoded}.{extension}"))
        })
    });
    beside.into_iter().chain(neovim).flatten().collect()
}

pub fn normalise(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normal.pop();
            }
            other => normal.push(other),
        }
    }
    normal
}

pub fn named_in_arguments(arguments: &[String], cwd: Option<&Path>, file: &Path) -> bool {
    let wanted = normalise(file);
    arguments
        .iter()
        .skip(1)
        .filter(|argument| !argument.starts_with('-') && !argument.starts_with('+'))
        .map(|argument| without_position(argument))
        .map(|argument| match cwd {
            Some(cwd) => cwd.join(argument),
            None => PathBuf::from(argument),
        })
        .any(|named| normalise(&named) == wanted)
}

fn without_position(argument: &str) -> &str {
    let mut rest = argument;
    for _ in 0..2 {
        match rest.rsplit_once(':') {
            Some((before, after))
                if !after.is_empty() && after.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                rest = before;
            }
            _ => break,
        }
    }
    rest
}

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

pub struct OnDisk<'a, T> {
    table: &'a T,
    home: Option<PathBuf>,
    state_home: Option<PathBuf>,
}

impl<'a, T: ProcessTable> OnDisk<'a, T> {
    pub fn new(table: &'a T, env: &impl Environment) -> Self {
        let home = env.var("HOME").map(PathBuf::from);
        let state_home = env
            .var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|home| home.join(".local/state")));
        Self {
            table,
            home,
            state_home,
        }
    }

    pub fn swap_file(&self, editor: &Editor, file: &Path) -> Option<PathBuf> {
        let family: Vec<Pid> = std::iter::once(editor.identity.pid)
            .chain(
                self.table
                    .children(editor.identity.pid)
                    .iter()
                    .map(|child| child.pid()),
            )
            .collect();
        let wanted = normalise(file);
        swap_candidates(&wanted, self.state_home.as_deref())
            .into_iter()
            .find(|candidate| {
                read_header(candidate)
                    .and_then(|block| parse_swap_header(&block, self.home.as_deref()))
                    .is_some_and(|owner| {
                        family.contains(&owner.pid) && normalise(&owner.file) == wanted
                    })
            })
    }
}

fn read_header(path: &Path) -> Option<Vec<u8>> {
    let mut block = Vec::new();
    File::open(path)
        .ok()?
        .take(SWAP_HEADER_BYTES)
        .read_to_end(&mut block)
        .ok()?;
    Some(block)
}

impl<T: ProcessTable> LoadedFiles for OnDisk<'_, T> {
    fn holds(&self, editor: &Editor, file: &Path) -> bool {
        match editor.kind {
            EditorKind::Vim | EditorKind::Neovim => self.swap_file(editor, file).is_some(),
            EditorKind::Helix => {
                let pid = editor.identity.pid;
                named_in_arguments(
                    &self.table.arguments(pid),
                    self.table.cwd(pid).as_deref(),
                    file,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::super::process::fake::FakeTable;
    use super::super::process::{Identity, StartTime};
    use super::*;

    fn header(pid: u32, file: &str) -> Vec<u8> {
        let mut block = vec![0; 1024];
        block[..9].copy_from_slice(b"b0VIM 9.1");
        block[SWAP_PID_AT..SWAP_PID_AT + 4].copy_from_slice(&pid.to_le_bytes());
        block[28..31].copy_from_slice(b"ada");
        block[SWAP_FILE_AT..SWAP_FILE_AT + file.len()].copy_from_slice(file.as_bytes());
        block
    }

    fn editor(kind: EditorKind, pid: u32) -> Editor {
        Editor {
            kind,
            identity: Identity {
                pid: Pid(pid),
                start: StartTime(1),
            },
            socket: None,
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("termleaf-evidence-{name}-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("a scratch directory");
        directory
    }

    struct Home(PathBuf);

    impl Environment for Home {
        fn var(&self, name: &str) -> Option<String> {
            (name == "HOME").then(|| self.0.to_string_lossy().into_owned())
        }
    }

    #[test]
    fn a_swap_header_under_home_is_expanded() {
        assert_eq!(
            parse_swap_header(&header(7, "~/thesis/ch5.tex"), Some(Path::new("/home/ada")))
                .map(|owner| owner.file),
            Some(PathBuf::from("/home/ada/thesis/ch5.tex"))
        );
    }

    #[test]
    fn something_that_is_not_a_swap_file_has_no_owner() {
        assert_eq!(parse_swap_header(b"%PDF-1.7", None), None);
        assert_eq!(parse_swap_header(&header(7, "")[..200], None), None);
    }

    #[test]
    fn helix_arguments_name_files_with_or_without_a_position() {
        let arguments: Vec<String> = ["hx", "--vsplit", "ch5.tex:77:3", "../notes.md"]
            .map(str::to_owned)
            .to_vec();
        let cwd = Some(Path::new("/tmp/thesis/chapters"));
        assert!(named_in_arguments(
            &arguments,
            cwd,
            Path::new("/tmp/thesis/chapters/ch5.tex")
        ));
        assert!(named_in_arguments(
            &arguments,
            cwd,
            Path::new("/tmp/thesis/notes.md")
        ));
        assert!(!named_in_arguments(
            &arguments,
            cwd,
            Path::new("/tmp/thesis/chapters/ch1.tex")
        ));
    }

    #[test]
    fn vim_holds_a_file_whose_swap_file_names_its_pid() {
        let directory = scratch("vim");
        let file = directory.join("ch5.tex");
        fs::write(
            directory.join(".ch5.tex.swp"),
            header(70, &file.to_string_lossy()),
        )
        .expect("a swap file");
        let mut table = FakeTable::default();
        table.spawn(70, 60, "vim.gtk3").spawn(71, 60, "vim");
        let disk = OnDisk::new(&table, &Home(directory.clone()));
        assert!(disk.holds(&editor(EditorKind::Vim, 70), &file));
        assert!(!disk.holds(&editor(EditorKind::Vim, 71), &file));
        assert!(!disk.holds(&editor(EditorKind::Vim, 70), &directory.join("ch1.tex")));
        fs::remove_dir_all(&directory).expect("cleanup");
    }

    #[test]
    fn neovim_holds_a_file_whose_swap_file_names_its_embedded_child() {
        let directory = scratch("nvim");
        let file = directory.join("ch5.tex");
        let swap_directory = directory.join(".local/state/nvim/swap");
        fs::create_dir_all(&swap_directory).expect("a swap directory");
        let encoded = file.to_string_lossy().replace('/', "%");
        fs::write(
            swap_directory.join(format!("{encoded}.swp")),
            header(81, &file.to_string_lossy()),
        )
        .expect("a swap file");
        let mut table = FakeTable::default();
        table.spawn(80, 60, "nvim").spawn(81, 80, "nvim");
        let disk = OnDisk::new(&table, &Home(directory.clone()));
        assert_eq!(
            disk.swap_file(&editor(EditorKind::Neovim, 80), &file),
            Some(swap_directory.join(format!("{encoded}.swp")))
        );
        fs::remove_dir_all(&directory).expect("cleanup");
    }

    #[test]
    fn helix_holds_the_files_it_was_started_with() {
        let mut table = FakeTable::default();
        table
            .spawn(90, 60, "hx")
            .argv(90, &["hx", "ch5.tex"], "/tmp/thesis");
        let disk = OnDisk::new(&table, &Home(PathBuf::from("/home/ada")));
        let helix = editor(EditorKind::Helix, 90);
        assert!(disk.holds(&helix, Path::new("/tmp/thesis/ch5.tex")));
        assert!(!disk.holds(&helix, Path::new("/tmp/thesis/ch1.tex")));
    }
}
