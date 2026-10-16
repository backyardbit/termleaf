use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;

pub fn osc52(text: &str) -> String {
    let encoded = base64_simd::STANDARD.encode_to_string(text.as_bytes());
    format!("\x1b]52;c;{encoded}\x07")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Linux,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Linux
        }
    }
}

pub fn native_command(
    platform: Platform,
    variable: impl Fn(&str) -> Option<String>,
) -> Option<&'static [&'static str]> {
    let set = |name: &str| variable(name).is_some_and(|value| !value.is_empty());
    if set("SSH_CONNECTION") || set("SSH_TTY") {
        return None;
    }
    match platform {
        Platform::MacOs => Some(&["pbcopy"]),
        Platform::Linux if set("WAYLAND_DISPLAY") => Some(&["wl-copy"]),
        Platform::Linux if set("DISPLAY") => Some(&["xclip", "-selection", "clipboard"]),
        Platform::Linux => None,
    }
}

pub trait Clipboard {
    fn copy(&self, text: &str);
}

pub struct SystemClipboard {
    command: Option<&'static [&'static str]>,
}

impl SystemClipboard {
    pub fn detect() -> Self {
        Self {
            command: native_command(Platform::current(), |name| std::env::var(name).ok()),
        }
    }
}

impl Clipboard for SystemClipboard {
    fn copy(&self, text: &str) {
        let Some((program, arguments)) = self.command.and_then(<[_]>::split_first) else {
            return;
        };
        let text = text.to_owned();
        thread::spawn(move || {
            let Ok(mut child) = Command::new(program)
                .args(arguments)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            else {
                return;
            };
            if let Some(mut input) = child.stdin.take() {
                let _ = input.write_all(text.as_bytes());
            }
            let _ = child.wait();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn osc52_sends_the_text_base64_encoded_to_the_clipboard() {
        assert_eq!(osc52("héllo"), "\x1b]52;c;aMOpbGxv\x07");
    }

    #[test]
    fn a_local_session_also_copies_with_the_platform_tool() {
        assert_eq!(
            native_command(Platform::MacOs, environment(&[])),
            Some(&["pbcopy"][..])
        );
        assert_eq!(
            native_command(
                Platform::Linux,
                environment(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")])
            ),
            Some(&["wl-copy"][..])
        );
        assert_eq!(
            native_command(Platform::Linux, environment(&[("DISPLAY", ":0")])),
            Some(&["xclip", "-selection", "clipboard"][..])
        );
        assert_eq!(native_command(Platform::Linux, environment(&[])), None);
    }

    #[test]
    fn over_ssh_only_the_terminal_gets_the_text() {
        assert_eq!(
            native_command(
                Platform::MacOs,
                environment(&[("SSH_CONNECTION", "10.0.0.1 22 10.0.0.2 22")])
            ),
            None
        );
        assert_eq!(
            native_command(
                Platform::Linux,
                environment(&[("SSH_TTY", "/dev/pts/1"), ("DISPLAY", ":0")])
            ),
            None
        );
    }
}
