use std::io;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const NOTICE_FOR: Duration = Duration::from_secs(4);
const WEB_SCHEMES: [&str; 3] = ["http:", "https:", "mailto:"];

#[cfg(target_os = "macos")]
const OPENER: &str = "open";
#[cfg(not(target_os = "macos"))]
const OPENER: &str = "xdg-open";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebAddress(String);

#[derive(Debug, PartialEq, Eq)]
pub struct NotWeb;

impl TryFrom<&str> for WebAddress {
    type Error = NotWeb;

    fn try_from(uri: &str) -> Result<Self, NotWeb> {
        let web = WEB_SCHEMES.iter().any(|scheme| {
            uri.get(..scheme.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
        });
        let plain = !uri.chars().any(|c| c.is_whitespace() || c.is_control());
        (web && plain && uri.len() > uri.find(':').map_or(0, |colon| colon + 1))
            .then(|| Self(uri.to_owned()))
            .ok_or(NotWeb)
    }
}

impl WebAddress {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub trait Browser {
    fn open(&mut self, address: &WebAddress) -> io::Result<()>;
}

pub struct SystemBrowser;

impl Browser for SystemBrowser {
    fn open(&mut self, address: &WebAddress) -> io::Result<()> {
        let mut opener = Command::new(OPENER)
            .arg(address.as_str())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        thread::spawn(move || opener.wait());
        Ok(())
    }
}

pub struct WebLinks {
    browser: Box<dyn Browser>,
    notice: Option<(String, Instant)>,
}

impl WebLinks {
    pub fn new(browser: Box<dyn Browser>) -> Self {
        Self {
            browser,
            notice: None,
        }
    }

    pub fn open(&mut self, address: &WebAddress, now: Instant) {
        let text = match self.browser.open(address) {
            Ok(()) => format!("opening {}", address.as_str()),
            Err(problem) => format!("could not open {}: {problem}", address.as_str()),
        };
        self.notice = Some((text, now + NOTICE_FOR));
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_ref().map(|(text, _)| text.as_str())
    }

    pub fn notice_ends(&self) -> Option<Instant> {
        self.notice.as_ref().map(|(_, until)| *until)
    }

    pub fn expire(&mut self, now: Instant) {
        if self.notice_ends().is_some_and(|until| now >= until) {
            self.notice = None;
        }
    }
}

#[cfg(test)]
pub mod fake {
    use std::cell::RefCell;
    use std::io;
    use std::rc::Rc;

    use super::{Browser, WebAddress};

    #[derive(Default, Clone)]
    pub struct RecordingBrowser {
        pub opened: Rc<RefCell<Vec<WebAddress>>>,
        pub fails: bool,
    }

    impl Browser for RecordingBrowser {
        fn open(&mut self, address: &WebAddress) -> io::Result<()> {
            if self.fails {
                return Err(io::Error::new(io::ErrorKind::NotFound, "no opener"));
            }
            self.opened.borrow_mut().push(address.clone());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::RecordingBrowser;
    use super::*;

    fn address(uri: &str) -> WebAddress {
        WebAddress::try_from(uri).unwrap()
    }

    #[test]
    fn web_and_mail_addresses_are_accepted() {
        for uri in [
            "https://example.com/a?b=c#d",
            "http://example.com",
            "HTTPS://EXAMPLE.COM",
            "mailto:someone@example.com",
        ] {
            assert_eq!(WebAddress::try_from(uri).map(|a| a.0), Ok(uri.to_owned()));
        }
    }

    #[test]
    fn addresses_that_could_run_or_read_local_things_are_refused() {
        for uri in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "/usr/bin/true",
            "--help",
            "ssh://host",
            "https:",
            "https://example.com/a b",
            "https://example.com/\n",
            "",
        ] {
            assert_eq!(WebAddress::try_from(uri), Err(NotWeb), "{uri:?}");
        }
    }

    #[test]
    fn opening_a_link_hands_it_to_the_browser_and_says_so() {
        let browser = RecordingBrowser::default();
        let mut links = WebLinks::new(Box::new(browser.clone()));
        let now = Instant::now();
        links.open(&address("https://example.com"), now);
        assert_eq!(*browser.opened.borrow(), [address("https://example.com")]);
        assert_eq!(links.notice(), Some("opening https://example.com"));
        assert_eq!(links.notice_ends(), Some(now + NOTICE_FOR));
    }

    #[test]
    fn a_browser_that_cannot_start_is_reported() {
        let browser = RecordingBrowser {
            fails: true,
            ..RecordingBrowser::default()
        };
        let mut links = WebLinks::new(Box::new(browser));
        links.open(&address("https://example.com"), Instant::now());
        assert_eq!(
            links.notice(),
            Some("could not open https://example.com: no opener")
        );
    }

    #[test]
    fn the_notice_goes_away_after_a_while() {
        let mut links = WebLinks::new(Box::new(RecordingBrowser::default()));
        let now = Instant::now();
        links.open(&address("https://example.com"), now);
        links.expire(now + NOTICE_FOR - Duration::from_millis(1));
        assert!(links.notice().is_some());
        links.expire(now + NOTICE_FOR);
        assert_eq!(links.notice(), None);
    }
}
