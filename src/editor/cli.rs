use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

const REPLY_WITHIN: Duration = Duration::from_secs(2);

pub trait Cli {
    fn run(&self, program: &str, arguments: &[String]) -> Result<String>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemCli;

fn drain(stream: Option<impl Read + Send + 'static>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut output = Vec::new();
        if let Some(mut stream) = stream {
            let _ = stream.read_to_end(&mut output);
        }
        output
    })
}

impl Cli for SystemCli {
    fn run(&self, program: &str, arguments: &[String]) -> Result<String> {
        let mut child = Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("could not run {program}"))?;
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let deadline = Instant::now() + REPLY_WITHIN;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                bail!(
                    "{program} did not answer within {} s",
                    REPLY_WITHIN.as_secs()
                );
            }
            thread::sleep(Duration::from_millis(1));
        };
        let stdout = stdout.join().unwrap_or_default();
        let stderr = stderr.join().unwrap_or_default();
        if !status.success() {
            bail!(
                "{program} {} failed: {}",
                arguments.join(" "),
                String::from_utf8_lossy(&stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&stdout).into_owned())
    }
}

pub fn owned(arguments: &[&str]) -> Vec<String> {
    arguments
        .iter()
        .map(|argument| (*argument).to_owned())
        .collect()
}

#[cfg(test)]
pub mod fake {
    use std::cell::RefCell;

    use anyhow::{Result, bail};

    use super::Cli;

    enum Reply {
        Print(String),
        Fail,
        WriteLastArgument(String),
    }

    #[derive(Default)]
    pub struct FakeCli {
        calls: RefCell<Vec<Vec<String>>>,
        replies: Vec<(Vec<String>, Reply)>,
    }

    impl FakeCli {
        pub fn prints(mut self, when: &[&str], output: &str) -> Self {
            self.replies
                .push((super::owned(when), Reply::Print(output.to_owned())));
            self
        }

        pub fn fails(mut self, when: &[&str]) -> Self {
            self.replies.push((super::owned(when), Reply::Fail));
            self
        }

        pub fn writes_last_argument(mut self, when: &[&str], content: &str) -> Self {
            self.replies.push((
                super::owned(when),
                Reply::WriteLastArgument(content.to_owned()),
            ));
            self
        }

        pub fn calls(&self) -> Vec<Vec<String>> {
            self.calls.borrow().clone()
        }
    }

    impl Cli for FakeCli {
        fn run(&self, program: &str, arguments: &[String]) -> Result<String> {
            let mut call = vec![program.to_owned()];
            call.extend(arguments.iter().cloned());
            self.calls.borrow_mut().push(call);
            let reply = self.replies.iter().find(|(when, _)| {
                when.iter()
                    .all(|needle| arguments.contains(needle) || needle == program)
            });
            match reply {
                None => Ok(String::new()),
                Some((_, Reply::Print(output))) => Ok(output.clone()),
                Some((_, Reply::Fail)) => bail!("{program} failed"),
                Some((_, Reply::WriteLastArgument(content))) => {
                    std::fs::write(arguments.last().map_or("", String::as_str), content)?;
                    Ok(String::new())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_cli_returns_what_the_program_prints() {
        assert_eq!(
            SystemCli
                .run("sh", &owned(&["-c", "printf 'a b'"]))
                .expect("sh runs"),
            "a b"
        );
    }

    #[test]
    fn a_failing_program_is_an_error_that_carries_its_message() {
        let error = SystemCli
            .run(
                "sh",
                &owned(&["-c", "echo 'No screen session found.' >&2; exit 1"]),
            )
            .expect_err("a non-zero exit");
        assert!(error.to_string().contains("No screen session found."));
        assert!(SystemCli.run("termleaf-no-such-program", &[]).is_err());
    }

    #[test]
    fn a_program_that_hangs_is_killed_after_the_deadline() {
        let started = Instant::now();
        let error = SystemCli
            .run("sleep", &owned(&["30"]))
            .expect_err("sleep outlives the deadline");
        assert!(error.to_string().contains("did not answer within 2 s"));
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
