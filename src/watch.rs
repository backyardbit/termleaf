use std::ffi::OsString;
use std::path::Path;

use anyhow::{Context, Result};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

pub fn watch(path: &Path, on_change: impl Fn() + Send + 'static) -> Result<RecommendedWatcher> {
    let file_name: OsString = path
        .file_name()
        .context("the path has no file name")?
        .into();
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        if let Ok(event) = event
            && touches(&event, &file_name)
        {
            on_change();
        }
    })?;
    watcher
        .watch(directory, RecursiveMode::NonRecursive)
        .with_context(|| format!("watching {}", directory.display()))?;
    Ok(watcher)
}

fn touches(event: &Event, file_name: &OsString) -> bool {
    let relevant_kind = matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) | EventKind::Any
    );
    relevant_kind
        && event
            .paths
            .iter()
            .any(|changed| changed.file_name() == Some(file_name.as_os_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::mpsc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn scratch_directory() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let directory = std::env::temp_dir().join(format!("termleaf-watch-{nanos}"));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn event(kind: EventKind, path: &str) -> Event {
        Event::new(kind).add_path(path.into())
    }

    #[test]
    fn matches_changes_to_the_watched_file() {
        let name = OsString::from("doc.pdf");
        let write = EventKind::Modify(notify::event::ModifyKind::Any);
        assert!(touches(&event(write, "/tmp/doc.pdf"), &name));
    }

    #[test]
    fn ignores_other_files_in_the_directory() {
        let name = OsString::from("doc.pdf");
        let write = EventKind::Modify(notify::event::ModifyKind::Any);
        assert!(!touches(&event(write, "/tmp/doc.aux"), &name));
    }

    #[test]
    fn ignores_reads() {
        let name = OsString::from("doc.pdf");
        let read = EventKind::Access(notify::event::AccessKind::Any);
        assert!(!touches(&event(read, "/tmp/doc.pdf"), &name));
    }

    #[test]
    fn reports_writes_to_the_watched_file() {
        let directory = scratch_directory();
        let target = directory.join("doc.pdf");
        fs::write(&target, "one").unwrap();
        let (sender, changes) = mpsc::channel();
        let _watcher = watch(&target, move || {
            let _ = sender.send(());
        })
        .unwrap();

        std::thread::sleep(Duration::from_millis(200));
        fs::write(&target, "two").unwrap();
        assert!(changes.recv_timeout(Duration::from_secs(5)).is_ok());
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn reports_a_file_replaced_by_rename() {
        let directory = scratch_directory();
        let target = directory.join("doc.pdf");
        fs::write(&target, "one").unwrap();
        let (sender, changes) = mpsc::channel();
        let _watcher = watch(&target, move || {
            let _ = sender.send(());
        })
        .unwrap();

        std::thread::sleep(Duration::from_millis(200));
        let staging = directory.join("doc.pdf.tmp");
        fs::write(&staging, "two").unwrap();
        fs::rename(&staging, &target).unwrap();
        assert!(changes.recv_timeout(Duration::from_secs(5)).is_ok());
        fs::remove_dir_all(&directory).unwrap();
    }
}
