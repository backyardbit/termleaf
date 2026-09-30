const MODES: [&str; 3] = ["NOR", "INS", "SEL"];

pub fn statusline(screen: &str) -> Option<(&str, u32)> {
    screen
        .lines()
        .rev()
        .flat_map(|row| row.split('│'))
        .find_map(position)
}

fn position(view: &str) -> Option<(&str, u32)> {
    let view = view.trim_start();
    let rest = MODES
        .iter()
        .find_map(|mode| view.strip_prefix(mode)?.strip_prefix(' '))?
        .trim_start();
    let name = rest.split("  ").next()?.split(" [").next()?;
    let (line, _) = rest.split_whitespace().last()?.split_once(':')?;
    let line = line.parse().ok().filter(|line| *line > 0)?;
    (!name.is_empty()).then_some((name, line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_focused_view_gives_its_file_and_line() {
        let wide = format!(" NOR   chapters/method.tex{}1 sel  60:1\n", " ".repeat(80));
        assert_eq!(statusline(&wide), Some(("chapters/method.tex", 60)));
        let split = "       chapters/method.tex        1 sel  60:1 │ INS   chapters/intro.tex [+]        1 sel  1:3\n\n";
        assert_eq!(statusline(split), Some(("chapters/intro.tex", 1)));
        let selecting = "  65  Editor measure\n SEL   /tmp/thesis/ch5.tex   2 sels  40:\n";
        assert_eq!(statusline(selecting), Some(("/tmp/thesis/ch5.tex", 40)));
    }

    #[test]
    fn a_covered_or_unlabelled_statusline_gives_nothing() {
        for screen in [
            "get-option     buffer-previous\n:o\n",
            "       chapters/method.tex        1 sel  60:1\n",
            " NOR   chapters/method.tex   1 sel  0:1\n",
            " NORMAL   x  3:1\n",
        ] {
            assert_eq!(statusline(screen), None, "{screen:?}");
        }
    }
}
