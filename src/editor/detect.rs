use std::path::PathBuf;

pub trait Environment {
    fn var(&self, name: &str) -> Option<String>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnvironment;

impl Environment for ProcessEnvironment {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|value| !value.is_empty())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MultiplexerKind {
    Tmux,
    Zellij,
    Screen,
    Herdr,
    Kitty,
    Wezterm,
    Konsole,
}

const INNERMOST_FIRST: [MultiplexerKind; 7] = [
    MultiplexerKind::Tmux,
    MultiplexerKind::Zellij,
    MultiplexerKind::Screen,
    MultiplexerKind::Herdr,
    MultiplexerKind::Kitty,
    MultiplexerKind::Wezterm,
    MultiplexerKind::Konsole,
];

impl MultiplexerKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Tmux => "tmux",
            Self::Zellij => "zellij",
            Self::Screen => "screen",
            Self::Herdr => "herdr",
            Self::Kitty => "kitty",
            Self::Wezterm => "wezterm",
            Self::Konsole => "konsole",
        }
    }

    fn servers(self) -> &'static [&'static str] {
        match self {
            Self::Tmux => &["tmux"],
            Self::Zellij => &["zellij"],
            Self::Screen => &["screen", "SCREEN"],
            Self::Herdr => &["herdr"],
            Self::Kitty => &["kitty"],
            Self::Wezterm => &["wezterm-gui", "wezterm-mux-server", "wezterm"],
            Self::Konsole => &["konsole"],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    pub kind: MultiplexerKind,
    pub own_pane: Option<String>,
    pub session: Option<String>,
    pub control: Option<PathBuf>,
}

impl Layer {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the kitty adapter shows the hint in a later PR of the stack"
        )
    )]
    pub fn hint(&self) -> Option<&'static str> {
        match (self.kind, &self.control) {
            (MultiplexerKind::Kitty, None) => Some("kitty remote control is off"),
            _ => None,
        }
    }

    fn read(kind: MultiplexerKind, env: &impl Environment) -> Option<Self> {
        let var = |name: &str| env.var(name);
        let layer = |own_pane: Option<String>, session: Option<String>, control: Option<String>| {
            Some(Self {
                kind,
                own_pane,
                session,
                control: control.map(PathBuf::from),
            })
        };
        match kind {
            MultiplexerKind::Tmux => {
                let tmux = var("TMUX")?;
                layer(var("TMUX_PANE"), None, tmux_socket(&tmux))
            }
            MultiplexerKind::Zellij => {
                var("ZELLIJ")?;
                let pane = var("ZELLIJ_PANE_ID").map(|id| format!("terminal_{id}"));
                layer(pane, var("ZELLIJ_SESSION_NAME"), None)
            }
            MultiplexerKind::Screen => {
                let sty = var("STY")?;
                layer(var("WINDOW"), Some(sty), None)
            }
            MultiplexerKind::Herdr => {
                let pane = var("HERDR_PANE_ID");
                if var("HERDR_ENV").is_none() && pane.is_none() {
                    return None;
                }
                layer(pane, var("HERDR_SESSION"), var("HERDR_SOCKET_PATH"))
            }
            MultiplexerKind::Kitty => {
                let window = var("KITTY_WINDOW_ID")?;
                let listen = var("KITTY_LISTEN_ON")
                    .map(|to| to.strip_prefix("unix:").map_or(to.clone(), str::to_owned));
                layer(Some(window), None, listen)
            }
            MultiplexerKind::Wezterm => {
                let pane = var("WEZTERM_PANE")?;
                layer(Some(pane), None, var("WEZTERM_UNIX_SOCKET"))
            }
            MultiplexerKind::Konsole => {
                let service = var("KONSOLE_DBUS_SERVICE")?;
                layer(var("KONSOLE_DBUS_SESSION"), None, Some(service))
            }
        }
    }
}

fn tmux_socket(tmux: &str) -> Option<String> {
    tmux.rsplitn(3, ',')
        .nth(2)
        .filter(|socket| !socket.is_empty())
        .map(str::to_owned)
}

pub fn detect(env: &impl Environment, ancestors: &[String]) -> Vec<Layer> {
    let mut layers: Vec<Layer> = INNERMOST_FIRST
        .into_iter()
        .filter_map(|kind| Layer::read(kind, env))
        .collect();
    layers.sort_by_key(|layer| {
        ancestors
            .iter()
            .position(|program| layer.kind.servers().contains(&program.as_str()))
            .unwrap_or(usize::MAX)
    });
    layers
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    struct FakeEnvironment(HashMap<&'static str, &'static str>);

    impl FakeEnvironment {
        fn new(pairs: &[(&'static str, &'static str)]) -> Self {
            Self(pairs.iter().copied().collect())
        }
    }

    impl Environment for FakeEnvironment {
        fn var(&self, name: &str) -> Option<String> {
            self.0.get(name).map(|value| (*value).to_owned())
        }
    }

    fn kinds(layers: &[Layer]) -> Vec<MultiplexerKind> {
        layers.iter().map(|layer| layer.kind).collect()
    }

    #[test]
    fn multiplexers_are_named_like_their_commands() {
        assert_eq!(
            INNERMOST_FIRST.map(MultiplexerKind::name),
            [
                "tmux", "zellij", "screen", "herdr", "kitty", "wezterm", "konsole"
            ]
        );
    }

    #[test]
    fn zellij_panes_are_named_like_its_cli_names_them() {
        let env = FakeEnvironment::new(&[
            ("ZELLIJ", "0"),
            ("ZELLIJ_SESSION_NAME", "thesis"),
            ("ZELLIJ_PANE_ID", "2"),
        ]);
        let layers = detect(&env, &[]);
        assert_eq!(layers[0].own_pane.as_deref(), Some("terminal_2"));
        assert_eq!(layers[0].session.as_deref(), Some("thesis"));
    }

    #[test]
    fn screen_and_herdr_are_found_by_their_session_variables() {
        let env = FakeEnvironment::new(&[
            ("STY", "4242.pts-3.box"),
            ("WINDOW", "1"),
            ("HERDR_ENV", "1"),
            ("HERDR_SESSION", "main"),
            ("HERDR_PANE_ID", "w1:p2"),
            ("HERDR_SOCKET_PATH", "/run/user/1000/herdr/main.sock"),
        ]);
        let layers = detect(&env, &[]);
        assert_eq!(
            kinds(&layers),
            [MultiplexerKind::Screen, MultiplexerKind::Herdr]
        );
        assert_eq!(layers[0].own_pane.as_deref(), Some("1"));
        assert_eq!(layers[0].session.as_deref(), Some("4242.pts-3.box"));
        assert_eq!(layers[1].own_pane.as_deref(), Some("w1:p2"));
        assert_eq!(
            layers[1].control,
            Some(PathBuf::from("/run/user/1000/herdr/main.sock"))
        );
    }

    #[test]
    fn kitty_without_listen_on_says_remote_control_is_off() {
        let env = FakeEnvironment::new(&[("KITTY_WINDOW_ID", "7"), ("KITTY_PID", "900")]);
        let layers = detect(&env, &[]);
        assert_eq!(layers[0].own_pane.as_deref(), Some("7"));
        assert_eq!(layers[0].hint(), Some("kitty remote control is off"));
    }

    #[test]
    fn kitty_with_listen_on_has_a_socket_and_no_hint() {
        let env = FakeEnvironment::new(&[
            ("KITTY_WINDOW_ID", "7"),
            ("KITTY_LISTEN_ON", "unix:/tmp/kitty-900"),
        ]);
        let layers = detect(&env, &[]);
        assert_eq!(layers[0].control, Some(PathBuf::from("/tmp/kitty-900")));
        assert_eq!(layers[0].hint(), None);
    }

    #[test]
    fn wezterm_and_konsole_name_their_panes() {
        let env = FakeEnvironment::new(&[
            ("WEZTERM_PANE", "4"),
            ("WEZTERM_UNIX_SOCKET", "/run/user/1000/wezterm/sock"),
            ("KONSOLE_DBUS_SERVICE", "org.kde.konsole-311"),
            ("KONSOLE_DBUS_SESSION", "/Sessions/2"),
        ]);
        let layers = detect(&env, &[]);
        assert_eq!(
            kinds(&layers),
            [MultiplexerKind::Wezterm, MultiplexerKind::Konsole]
        );
        assert_eq!(layers[0].own_pane.as_deref(), Some("4"));
        assert_eq!(layers[1].own_pane.as_deref(), Some("/Sessions/2"));
        assert_eq!(
            layers[1].control,
            Some(PathBuf::from("org.kde.konsole-311"))
        );
    }

    #[test]
    fn the_process_tree_orders_nested_multiplexers_innermost_first() {
        let env = FakeEnvironment::new(&[
            ("TMUX", "/tmp/tmux-1000/default,4127,0"),
            ("TMUX_PANE", "%3"),
            ("HERDR_ENV", "1"),
            ("HERDR_PANE_ID", "w1:p2"),
            ("KITTY_WINDOW_ID", "7"),
        ]);
        let ancestors: Vec<String> = ["bash", "herdr", "bash", "tmux", "kitty"]
            .map(str::to_owned)
            .to_vec();
        assert_eq!(
            kinds(&detect(&env, &ancestors)),
            [
                MultiplexerKind::Herdr,
                MultiplexerKind::Tmux,
                MultiplexerKind::Kitty
            ]
        );
    }

    #[test]
    fn the_real_environment_has_no_value_for_an_unset_variable() {
        assert_eq!(ProcessEnvironment.var("TERMLEAF_NEVER_SET_ANYWHERE"), None);
    }
}
