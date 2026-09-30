use std::fs;

pub struct Tags<'a> {
    pub session_variable: &'a str,
    pub session: &'a str,
    pub pane_variable: &'a str,
    pub servers: &'a [&'a str],
}

fn stat_fields(pid: u32) -> Option<Vec<String>> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, after) = stat.rsplit_once(") ")?;
    Some(after.split_whitespace().map(str::to_owned).collect())
}

fn program(pid: u32) -> Option<String> {
    let exe = fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    Some(exe.file_name()?.to_string_lossy().into_owned())
}

fn has(pid: u32, name: &str, value: &str) -> bool {
    let wanted = format!("{name}={value}");
    fs::read(format!("/proc/{pid}/environ")).is_ok_and(|environ| {
        environ
            .split(|byte| *byte == 0)
            .any(|entry| entry == wanted.as_bytes())
    })
}

fn shell(tags: &Tags<'_>, pane: &str) -> Option<u32> {
    fs::read_dir("/proc")
        .ok()?
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .find(|pid| {
            has(*pid, tags.session_variable, tags.session)
                && has(*pid, tags.pane_variable, pane)
                && stat_fields(*pid)
                    .and_then(|fields| fields.get(1)?.parse::<u32>().ok())
                    .and_then(program)
                    .is_some_and(|parent| tags.servers.contains(&parent.as_str()))
        })
}

pub fn idle(tags: &Tags<'_>, pane: &str) -> bool {
    shell(tags, pane).is_some_and(|pid| {
        stat_fields(pid)
            .and_then(|fields| fields.get(5)?.parse::<u32>().ok())
            .is_some_and(|foreground| foreground == pid)
    })
}
