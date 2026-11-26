use std::fmt;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

pub const TOKEN_VARIABLE: &str = "OSC7577_TOKEN";
const TOKEN_LENGTHS: std::ops::RangeInclusive<usize> = 22..=64;
const MAX_ARGUMENTS: usize = 64;
const MAX_SEQUENCE_BYTES: usize = 2048;

pub struct Token(String);

#[derive(Debug, PartialEq, Eq)]
pub struct MalformedToken;

impl TryFrom<&str> for Token {
    type Error = MalformedToken;

    fn try_from(value: &str) -> Result<Self, MalformedToken> {
        let url_safe = value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
        (url_safe && TOKEN_LENGTHS.contains(&value.len()))
            .then(|| Self(value.to_owned()))
            .ok_or(MalformedToken)
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Token(redacted)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Owner {
    pub pid: u32,
    pub time: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resume {
    Run {
        argv: Vec<String>,
        cwd: Option<PathBuf>,
    },
    Clear,
}

pub struct Reporter {
    token: Token,
}

impl Reporter {
    pub fn new(token: Token) -> Self {
        Self { token }
    }

    pub fn send(&self, resume: &Resume, out: &mut impl Write) {
        if let Some(sequence) = report(resume, Owner::now(), &self.token) {
            let _ = out.write_all(sequence.as_bytes());
            let _ = out.flush();
        }
    }
}

impl Owner {
    pub fn now() -> Self {
        Self {
            pid: std::process::id(),
            time: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_secs()),
        }
    }
}

pub fn report(resume: &Resume, owner: Owner, token: &Token) -> Option<String> {
    let mut body = format!("pid={}:time={}", owner.pid, owner.time);
    match resume {
        Resume::Clear => body.push_str(":clear=1"),
        Resume::Run { argv, cwd } => {
            let program = argv.first()?;
            let all_printable = argv.iter().all(|argument| printable(argument));
            if !runnable(program) || !all_printable || argv.len() > MAX_ARGUMENTS {
                return None;
            }
            let encoded: Vec<String> = argv.iter().map(|argument| encode(argument)).collect();
            body.push_str(":argv=");
            body.push_str(&encoded.join(","));
            if let Some(cwd) = cwd {
                let directory = cwd
                    .to_str()
                    .filter(|directory| cwd.is_absolute() && printable(directory))?;
                body.push_str(":cwd=");
                body.push_str(&encode(directory));
            }
        }
    }
    let mac = Sha256::new()
        .chain_update(body.as_bytes())
        .chain_update(b":")
        .chain_update(token.0.as_bytes())
        .finalize();
    let mac: String = mac.iter().map(|byte| format!("{byte:02x}")).collect();
    let sequence = format!("\x1b]7577;{body}:mac={mac}\x1b\\");
    (sequence.len() <= MAX_SEQUENCE_BYTES).then_some(sequence)
}

fn runnable(program: &str) -> bool {
    let bare = !program.is_empty() && !program.starts_with('-') && !program.contains('/');
    bare || program.starts_with('/')
}

fn printable(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

fn encode(text: &str) -> String {
    base64_simd::STANDARD.encode_to_string(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: Owner = Owner {
        pid: 74877,
        time: 1_795_543_200,
    };

    fn token() -> Token {
        Token::try_from("dummy-token-replace-me").unwrap()
    }

    fn run(argv: &[&str], cwd: Option<&str>) -> Resume {
        Resume::Run {
            argv: argv.iter().map(|part| (*part).to_owned()).collect(),
            cwd: cwd.map(PathBuf::from),
        }
    }

    #[test]
    fn a_clear_report_matches_the_spec_vector() {
        assert_eq!(
            report(&Resume::Clear, OWNER, &token()).unwrap(),
            "\x1b]7577;pid=74877:time=1795543200:clear=1:mac=\
             c30603369f98fef581a4098be889175b0c74f0641e49f8b25f4326cb38f97c93\x1b\\"
        );
    }

    #[test]
    fn a_command_report_matches_the_spec_vector() {
        let resume = run(
            &[
                "nvim",
                "-S",
                "/home/user/.local/state/nvim/sessions/8f2c1d.vim",
            ],
            None,
        );
        assert_eq!(
            report(&resume, OWNER, &token()).unwrap(),
            "\x1b]7577;pid=74877:time=1795543200:\
             argv=bnZpbQ==,LVM=,L2hvbWUvdXNlci8ubG9jYWwvc3RhdGUvbnZpbS9zZXNzaW9ucy84ZjJjMWQudmlt:mac=\
             9dbb99bfb856749a87563839d7ce9062818d78cf25909db93f309e0cd3b1909b\x1b\\"
        );
    }

    #[test]
    fn a_report_with_a_working_directory_matches_the_spec_vector() {
        let resume = run(&["lazygit"], Some("/home/user/src/project"));
        assert_eq!(
            report(&resume, OWNER, &token()).unwrap(),
            "\x1b]7577;pid=74877:time=1795543200:argv=bGF6eWdpdA==:\
             cwd=L2hvbWUvdXNlci9zcmMvcHJvamVjdA==:mac=\
             35cb909805c380336efd3689a125acc774a9dec8422a1d21492ebec06ca758e1\x1b\\"
        );
    }

    #[test]
    fn a_token_is_22_to_64_url_safe_characters() {
        assert!(Token::try_from("a".repeat(22).as_str()).is_ok());
        assert!(Token::try_from("A-z_9".repeat(12).as_str()).is_ok());
        assert_eq!(
            Token::try_from("a".repeat(21).as_str()).err(),
            Some(MalformedToken)
        );
        assert_eq!(
            Token::try_from("a".repeat(65).as_str()).err(),
            Some(MalformedToken)
        );
        assert_eq!(
            Token::try_from("dummy token replace me").err(),
            Some(MalformedToken)
        );
        assert_eq!(
            Token::try_from("dummy:token;replace=me").err(),
            Some(MalformedToken)
        );
    }

    #[test]
    fn a_reporter_signs_its_own_process_at_the_current_time() {
        let mut written = Vec::new();
        Reporter::new(token()).send(&run(&["termleaf"], None), &mut written);
        let written = String::from_utf8(written).unwrap();
        let pid = format!("pid={}:", std::process::id());
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(
            written.starts_with(&format!("\x1b]7577;{pid}")),
            "{written}"
        );
        let sent: u64 = written
            .split(':')
            .find_map(|pair| pair.strip_prefix("time="))
            .unwrap()
            .parse()
            .unwrap();
        assert!(time.abs_diff(sent) <= 2, "{sent} vs {time}");
    }

    #[test]
    fn a_reporter_writes_nothing_for_a_command_it_cannot_report() {
        let mut written = Vec::new();
        Reporter::new(token()).send(&run(&["./termleaf"], None), &mut written);
        assert!(written.is_empty());
    }

    #[test]
    fn a_token_never_shows_in_debug_output() {
        assert!(!format!("{:?}", token()).contains("dummy"));
    }

    #[test]
    fn a_command_the_terminal_could_not_run_as_given_is_not_reported() {
        let unreportable = [
            run(&["bin/termleaf", "thesis.pdf"], None),
            run(&["-termleaf", "thesis.pdf"], None),
            run(&[""], None),
            run(&[], None),
            run(&["termleaf", "line\nbreak.pdf"], None),
            run(&["termleaf", "bell\u{7}.pdf"], None),
            run(&["termleaf", "c1\u{85}.pdf"], None),
            run(&["termleaf", "thesis.pdf"], Some("relative/dir")),
            run(&["termleaf", "thesis.pdf"], Some("/tab\tdir")),
        ];
        for resume in unreportable {
            assert_eq!(report(&resume, OWNER, &token()), None, "{resume:?}");
        }
    }

    #[test]
    fn an_absolute_program_and_unicode_arguments_are_reported() {
        let resume = run(&["/usr/local/bin/termleaf", "/home/ü/thèse.pdf"], None);
        assert!(report(&resume, OWNER, &token()).is_some());
    }

    #[test]
    fn a_report_stays_within_the_limits_every_terminal_accepts() {
        let many = vec!["x"; 65];
        assert_eq!(report(&run(&many, None), OWNER, &token()), None);
        assert!(report(&run(&many[..64], None), OWNER, &token()).is_some());
        let long = format!("/{}", "a".repeat(1500));
        assert_eq!(
            report(&run(&["termleaf", &long], None), OWNER, &token()),
            None
        );
    }
}
