use crate::pdf::PointRect;
use crate::search::Hit;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glyph {
    pub character: char,
    pub area: PointRect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextLine {
    pub block: usize,
    pub glyphs: Vec<Glyph>,
}

impl TextLine {
    fn top(&self) -> f32 {
        self.glyphs
            .iter()
            .map(|glyph| glyph.area.y0)
            .fold(f32::INFINITY, f32::min)
    }

    fn right(&self) -> f32 {
        self.glyphs
            .iter()
            .map(|glyph| glyph.area.x1)
            .fold(f32::NEG_INFINITY, f32::max)
    }

    fn shares_row_with(&self, other: &Self) -> bool {
        let overlap = self.bottom().min(other.bottom()) - self.top().max(other.top());
        let shorter = (self.bottom() - self.top()).min(other.bottom() - other.top());
        self.block == other.block
            && overlap > shorter / 2.0
            && other.top() < self.bottom()
            && other
                .glyphs
                .first()
                .is_some_and(|glyph| glyph.area.x0 >= self.right())
    }

    fn extend(&mut self, other: Self) {
        let (Some(last), Some(next)) = (self.glyphs.last(), other.glyphs.first()) else {
            return;
        };
        if !last.character.is_whitespace() && !next.character.is_whitespace() {
            let gap = Glyph {
                character: ' ',
                area: PointRect {
                    x0: last.area.x1,
                    y0: last.area.y0,
                    x1: next.area.x0,
                    y1: last.area.y1,
                },
            };
            self.glyphs.push(gap);
        }
        self.glyphs.extend(other.glyphs);
    }

    fn bottom(&self) -> f32 {
        self.glyphs
            .iter()
            .map(|glyph| glyph.area.y1)
            .fold(f32::NEG_INFINITY, f32::max)
    }

    fn is_space(&self, glyph: usize) -> bool {
        self.glyphs
            .get(glyph)
            .is_none_or(|glyph| glyph.character.is_whitespace())
    }

    fn starts_word(&self, glyph: usize) -> bool {
        !self.is_space(glyph) && (glyph == 0 || self.is_space(glyph - 1))
    }

    fn ends_word(&self, glyph: usize) -> bool {
        !self.is_space(glyph) && self.is_space(glyph + 1)
    }

    fn last(&self) -> usize {
        self.glyphs.len().saturating_sub(1)
    }

    fn nearest_to(&self, x: f32) -> usize {
        self.glyphs
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                (centre(a.area) - x)
                    .abs()
                    .total_cmp(&(centre(b.area) - x).abs())
            })
            .map_or(0, |(index, _)| index)
    }
}

fn centre(area: PointRect) -> f32 {
    f32::midpoint(area.x0, area.x1)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PageText {
    pub lines: Vec<TextLine>,
}

impl PageText {
    pub fn from_lines(lines: Vec<TextLine>) -> Self {
        let mut merged: Vec<TextLine> = Vec::new();
        for line in lines.into_iter().filter(|line| !line.glyphs.is_empty()) {
            match merged.last_mut() {
                Some(previous) if previous.shares_row_with(&line) => previous.extend(line),
                _ => merged.push(line),
            }
        }
        Self { lines: merged }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Caret {
    pub page: usize,
    pub line: usize,
    pub glyph: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordForward,
    WordBackward,
    WordEnd,
    LineStart,
    LineEnd,
    ParagraphForward,
    ParagraphBackward,
    DocumentStart,
    DocumentEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    Character,
    Line,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub from: Caret,
    pub to: Caret,
}

#[derive(Debug, Default)]
pub struct DocumentText {
    pages: Vec<Option<PageText>>,
}

impl DocumentText {
    pub fn new(page_count: usize) -> Self {
        Self {
            pages: vec![None; page_count],
        }
    }

    pub fn insert(&mut self, page: usize, text: PageText) {
        if let Some(slot) = self.pages.get_mut(page) {
            *slot = Some(text);
        }
    }

    pub fn missing(&self) -> impl Iterator<Item = usize> + '_ {
        self.pages
            .iter()
            .enumerate()
            .filter(|(_, text)| text.is_none())
            .map(|(page, _)| page)
    }

    fn page(&self, page: usize) -> Option<&PageText> {
        self.pages.get(page).and_then(Option::as_ref)
    }

    fn line(&self, page: usize, line: usize) -> Option<&TextLine> {
        self.page(page).and_then(|text| text.lines.get(line))
    }

    fn line_of(&self, caret: Caret) -> Option<&TextLine> {
        self.line(caret.page, caret.line)
    }

    fn glyph(&self, caret: Caret) -> Option<&Glyph> {
        self.line_of(caret)
            .and_then(|line| line.glyphs.get(caret.glyph))
    }

    pub fn area(&self, caret: Caret) -> Option<PointRect> {
        self.glyph(caret).map(|glyph| glyph.area)
    }

    pub fn first_below(&self, page: usize, y: f32) -> Option<Caret> {
        let text = self.page(page)?;
        match text.lines.iter().position(|line| line.bottom() > y) {
            Some(line) => Some(Caret {
                page,
                line,
                glyph: 0,
            }),
            None => self.first_after(page),
        }
    }

    fn first_after(&self, page: usize) -> Option<Caret> {
        let (page, line) = self.first_line_from(page + 1)?;
        Some(Caret {
            page,
            line,
            glyph: 0,
        })
    }

    fn first_line_from(&self, page: usize) -> Option<(usize, usize)> {
        let mut page = page;
        loop {
            if !self.page(page)?.lines.is_empty() {
                return Some((page, 0));
            }
            page += 1;
        }
    }

    fn line_after(&self, page: usize, line: usize) -> Option<(usize, usize)> {
        if self.line(page, line + 1).is_some() {
            return Some((page, line + 1));
        }
        self.first_line_from(page + 1)
    }

    fn line_before(&self, page: usize, line: usize) -> Option<(usize, usize)> {
        if line > 0 {
            return Some((page, line - 1));
        }
        let mut page = page.checked_sub(1)?;
        loop {
            let text = self.page(page)?;
            if let Some(last) = text.lines.len().checked_sub(1) {
                return Some((page, last));
            }
            page = page.checked_sub(1)?;
        }
    }

    fn forward(&self, caret: Caret) -> Option<Caret> {
        let line = self.line_of(caret)?;
        if caret.glyph + 1 < line.glyphs.len() {
            return Some(Caret {
                glyph: caret.glyph + 1,
                ..caret
            });
        }
        let (page, line) = self.line_after(caret.page, caret.line)?;
        Some(Caret {
            page,
            line,
            glyph: 0,
        })
    }

    fn backward(&self, caret: Caret) -> Option<Caret> {
        if caret.glyph > 0 {
            return Some(Caret {
                glyph: caret.glyph - 1,
                ..caret
            });
        }
        let (page, line) = self.line_before(caret.page, caret.line)?;
        let glyph = self.line(page, line)?.last();
        Some(Caret { page, line, glyph })
    }

    fn seek(
        &self,
        caret: Caret,
        step: impl Fn(&Self, Caret) -> Option<Caret>,
        stop: impl Fn(&TextLine, usize) -> bool,
    ) -> Caret {
        let mut at = caret;
        while let Some(next) = step(self, at) {
            at = next;
            if self.line_of(at).is_some_and(|line| stop(line, at.glyph)) {
                return at;
            }
        }
        caret
    }

    fn vertical(&self, caret: Caret, target: Option<(usize, usize)>) -> Caret {
        let (Some((page, line)), Some(glyph)) = (target, self.glyph(caret)) else {
            return caret;
        };
        let Some(text) = self.line(page, line) else {
            return caret;
        };
        Caret {
            page,
            line,
            glyph: text.nearest_to(centre(glyph.area)),
        }
    }

    fn paragraph_start(&self, caret: Caret) -> Caret {
        let Some(block) = self.line_of(caret).map(|line| line.block) else {
            return caret;
        };
        let mut line = caret.line;
        while line > 0
            && self
                .line(caret.page, line - 1)
                .is_some_and(|previous| previous.block == block)
        {
            line -= 1;
        }
        Caret {
            page: caret.page,
            line,
            glyph: 0,
        }
    }

    fn next_paragraph(&self, caret: Caret) -> Caret {
        let Some(block) = self.line_of(caret).map(|line| line.block) else {
            return caret;
        };
        let mut at = (caret.page, caret.line);
        while let Some(next) = self.line_after(at.0, at.1) {
            at = next;
            let changed = at.0 != caret.page
                || self
                    .line(at.0, at.1)
                    .is_some_and(|line| line.block != block);
            if changed {
                return Caret {
                    page: at.0,
                    line: at.1,
                    glyph: 0,
                };
            }
        }
        caret
    }

    fn previous_paragraph(&self, caret: Caret) -> Caret {
        let start = self.paragraph_start(caret);
        if start != caret {
            return start;
        }
        match self.backward(caret) {
            Some(previous) => self.paragraph_start(previous),
            None => caret,
        }
    }

    pub fn moved(&self, caret: Caret, motion: Motion) -> Caret {
        let line = self.line_of(caret);
        match motion {
            Motion::Right => self.forward(caret).unwrap_or(caret),
            Motion::Left => self.backward(caret).unwrap_or(caret),
            Motion::Down => self.vertical(caret, self.line_after(caret.page, caret.line)),
            Motion::Up => self.vertical(caret, self.line_before(caret.page, caret.line)),
            Motion::WordForward => self.seek(caret, Self::forward, TextLine::starts_word),
            Motion::WordBackward => self.seek(caret, Self::backward, TextLine::starts_word),
            Motion::WordEnd => self.seek(caret, Self::forward, TextLine::ends_word),
            Motion::LineStart => Caret { glyph: 0, ..caret },
            Motion::LineEnd => Caret {
                glyph: line.map_or(0, TextLine::last),
                ..caret
            },
            Motion::ParagraphForward => self.next_paragraph(caret),
            Motion::ParagraphBackward => self.previous_paragraph(caret),
            Motion::DocumentStart => self.furthest(caret, Self::backward),
            Motion::DocumentEnd => self.furthest(caret, Self::forward),
        }
    }

    fn furthest(&self, caret: Caret, step: impl Fn(&Self, Caret) -> Option<Caret>) -> Caret {
        let mut at = caret;
        while let Some(next) = step(self, at) {
            at = next;
        }
        at
    }

    pub fn span(&self, anchor: Caret, caret: Caret, granularity: Granularity) -> Span {
        let (from, to) = if anchor <= caret {
            (anchor, caret)
        } else {
            (caret, anchor)
        };
        match granularity {
            Granularity::Character => Span { from, to },
            Granularity::Line => Span {
                from: Caret { glyph: 0, ..from },
                to: Caret {
                    glyph: self.line_of(to).map_or(0, TextLine::last),
                    ..to
                },
            },
        }
    }

    fn lines_in(&self, span: Span) -> Vec<(usize, &TextLine, std::ops::RangeInclusive<usize>)> {
        let mut lines = Vec::new();
        let mut at = Some((span.from.page, span.from.line));
        while let Some((page, index)) = at {
            if (page, index) > (span.to.page, span.to.line) {
                break;
            }
            let Some(line) = self.line(page, index) else {
                break;
            };
            let first = if (page, index) == (span.from.page, span.from.line) {
                span.from.glyph
            } else {
                0
            };
            let last = if (page, index) == (span.to.page, span.to.line) {
                span.to.glyph.min(line.last())
            } else {
                line.last()
            };
            lines.push((page, line, first..=last));
            at = self.line_after(page, index);
        }
        lines
    }

    pub fn areas(&self, span: Span) -> Vec<Hit> {
        self.lines_in(span)
            .into_iter()
            .filter_map(|(page, line, glyphs)| {
                let chosen = line.glyphs.get(glyphs)?;
                let area = chosen
                    .iter()
                    .map(|glyph| glyph.area)
                    .reduce(|a, b| PointRect {
                        x0: a.x0.min(b.x0),
                        y0: a.y0.min(b.y0),
                        x1: a.x1.max(b.x1),
                        y1: a.y1.max(b.y1),
                    })?;
                Some(Hit {
                    page,
                    areas: vec![area],
                })
            })
            .collect()
    }

    pub fn text(&self, span: Span) -> String {
        let mut text = String::new();
        let mut previous: Option<(usize, usize)> = None;
        for (page, line, glyphs) in self.lines_in(span) {
            let Some(chosen) = line.glyphs.get(glyphs) else {
                continue;
            };
            let piece: String = chosen.iter().map(|glyph| glyph.character).collect();
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            if let Some(before) = previous {
                if before != (page, line.block) {
                    text.push('\n');
                } else if !text.ends_with('-') {
                    text.push(' ');
                }
            }
            text.push_str(piece);
            previous = Some((page, line.block));
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADVANCE: f32 = 10.0;
    const LEADING: f32 = 20.0;

    fn line(block: usize, row: usize, text: &str) -> TextLine {
        let top = f32::from(u16::try_from(row).unwrap()) * LEADING;
        TextLine {
            block,
            glyphs: text
                .chars()
                .zip(0u16..)
                .map(|(character, column)| {
                    let left = f32::from(column) * ADVANCE;
                    Glyph {
                        character,
                        area: PointRect {
                            x0: left,
                            y0: top,
                            x1: left + ADVANCE,
                            y1: top + LEADING / 2.0,
                        },
                    }
                })
                .collect(),
        }
    }

    fn page(paragraphs: &[&[&str]]) -> PageText {
        let mut lines = Vec::new();
        for (block, paragraph) in paragraphs.iter().enumerate() {
            for text in *paragraph {
                let row = lines.len();
                lines.push(line(block, row, text));
            }
        }
        PageText { lines }
    }

    fn document(pages: Vec<Option<PageText>>) -> DocumentText {
        DocumentText { pages }
    }

    fn sample() -> DocumentText {
        document(vec![
            Some(page(&[
                &["Hello world, this", "is a para-", "graph."],
                &["Second one"],
            ])),
            Some(PageText::default()),
            Some(page(&[&["Last page here"]])),
        ])
    }

    fn at(page: usize, line: usize, glyph: usize) -> Caret {
        Caret { page, line, glyph }
    }

    fn shifted(mut line: TextLine, columns: u16) -> TextLine {
        for glyph in &mut line.glyphs {
            glyph.area.x0 += f32::from(columns) * ADVANCE;
            glyph.area.x1 += f32::from(columns) * ADVANCE;
        }
        line
    }

    fn characters(line: &TextLine) -> String {
        line.glyphs.iter().map(|glyph| glyph.character).collect()
    }

    #[test]
    fn pieces_of_one_row_in_a_block_become_one_line_with_a_space_between() {
        let text = PageText::from_lines(vec![
            line(0, 0, "2"),
            shifted(line(0, 0, "Second"), 3),
            line(0, 1, "next row"),
            shifted(line(1, 1, "other block"), 10),
            line(2, 2, ""),
        ]);
        let lines: Vec<String> = text.lines.iter().map(characters).collect();
        assert_eq!(lines, ["2 Second", "next row", "other block"]);
        assert_eq!(text.lines[0].glyphs[1].area.x0, 10.0);
        assert_eq!(text.lines[0].glyphs[1].area.x1, 30.0);
    }

    #[test]
    fn right_and_left_walk_glyphs_and_wrap_between_lines() {
        let text = sample();
        assert_eq!(text.moved(at(0, 0, 0), Motion::Right), at(0, 0, 1));
        assert_eq!(text.moved(at(0, 0, 16), Motion::Right), at(0, 1, 0));
        assert_eq!(text.moved(at(0, 1, 0), Motion::Left), at(0, 0, 16));
        assert_eq!(text.moved(at(0, 0, 0), Motion::Left), at(0, 0, 0));
    }

    #[test]
    fn down_and_up_keep_the_caret_over_the_same_place_on_the_page() {
        let text = sample();
        assert_eq!(text.moved(at(0, 0, 3), Motion::Down), at(0, 1, 3));
        assert_eq!(text.moved(at(0, 0, 15), Motion::Down), at(0, 1, 9));
        assert_eq!(text.moved(at(0, 1, 3), Motion::Up), at(0, 0, 3));
    }

    #[test]
    fn line_motions_skip_pages_without_text() {
        let text = sample();
        assert_eq!(text.moved(at(0, 3, 2), Motion::Down), at(2, 0, 2));
        assert_eq!(text.moved(at(2, 0, 0), Motion::Up), at(0, 3, 0));
    }

    #[test]
    fn motions_stop_at_a_page_whose_text_has_not_arrived() {
        let text = document(vec![Some(page(&[&["only line"]])), None]);
        assert_eq!(text.moved(at(0, 0, 8), Motion::Right), at(0, 0, 8));
        assert_eq!(text.moved(at(0, 0, 2), Motion::Down), at(0, 0, 2));
    }

    #[test]
    fn w_b_and_e_move_by_words_across_lines_and_pages() {
        let text = sample();
        assert_eq!(text.moved(at(0, 0, 0), Motion::WordForward), at(0, 0, 6));
        assert_eq!(text.moved(at(0, 0, 13), Motion::WordForward), at(0, 1, 0));
        assert_eq!(text.moved(at(0, 3, 7), Motion::WordForward), at(2, 0, 0));
        assert_eq!(text.moved(at(0, 0, 6), Motion::WordBackward), at(0, 0, 0));
        assert_eq!(text.moved(at(0, 1, 0), Motion::WordBackward), at(0, 0, 13));
        assert_eq!(text.moved(at(0, 0, 0), Motion::WordEnd), at(0, 0, 4));
        assert_eq!(text.moved(at(0, 0, 4), Motion::WordEnd), at(0, 0, 11));
    }

    #[test]
    fn zero_and_dollar_go_to_the_ends_of_the_line() {
        let text = sample();
        assert_eq!(text.moved(at(0, 1, 5), Motion::LineStart), at(0, 1, 0));
        assert_eq!(text.moved(at(0, 1, 5), Motion::LineEnd), at(0, 1, 9));
    }

    #[test]
    fn braces_move_between_paragraphs() {
        let text = sample();
        assert_eq!(
            text.moved(at(0, 1, 4), Motion::ParagraphForward),
            at(0, 3, 0)
        );
        assert_eq!(
            text.moved(at(0, 3, 0), Motion::ParagraphForward),
            at(2, 0, 0)
        );
        assert_eq!(
            text.moved(at(0, 2, 3), Motion::ParagraphBackward),
            at(0, 0, 0)
        );
        assert_eq!(
            text.moved(at(0, 3, 0), Motion::ParagraphBackward),
            at(0, 0, 0)
        );
    }

    #[test]
    fn gg_and_capital_g_reach_the_ends_of_the_text() {
        let text = sample();
        assert_eq!(text.moved(at(0, 2, 3), Motion::DocumentEnd), at(2, 0, 13));
        assert_eq!(text.moved(at(2, 0, 3), Motion::DocumentStart), at(0, 0, 0));
    }

    #[test]
    fn the_first_caret_is_the_first_line_reaching_below_the_top_of_the_view() {
        let text = sample();
        assert_eq!(text.first_below(0, 25.0), Some(at(0, 1, 0)));
        assert_eq!(text.first_below(0, 500.0), Some(at(2, 0, 0)));
        assert_eq!(text.first_below(1, 0.0), Some(at(2, 0, 0)));
        assert_eq!(document(vec![None]).first_below(0, 0.0), None);
    }

    #[test]
    fn a_character_span_runs_from_the_earlier_end_to_the_later_one_inclusive() {
        let text = sample();
        let span = text.span(at(0, 1, 3), at(0, 0, 6), Granularity::Character);
        assert_eq!(
            span,
            Span {
                from: at(0, 0, 6),
                to: at(0, 1, 3)
            }
        );
        assert_eq!(text.text(span), "world, this is a");
    }

    #[test]
    fn a_line_span_covers_whole_lines() {
        let text = sample();
        let span = text.span(at(0, 0, 6), at(0, 1, 3), Granularity::Line);
        assert_eq!(text.text(span), "Hello world, this is a para-");
    }

    #[test]
    fn copied_text_joins_a_paragraph_and_breaks_between_paragraphs_and_pages() {
        let text = sample();
        let span = text.span(at(0, 0, 0), at(2, 0, 3), Granularity::Character);
        assert_eq!(
            text.text(span),
            "Hello world, this is a para-graph.\nSecond one\nLast"
        );
    }

    #[test]
    fn each_selected_line_is_one_highlighted_area_around_its_glyphs() {
        let text = sample();
        let span = text.span(at(0, 0, 6), at(0, 1, 1), Granularity::Character);
        assert_eq!(
            text.areas(span),
            [
                Hit {
                    page: 0,
                    areas: vec![PointRect {
                        x0: 60.0,
                        y0: 0.0,
                        x1: 170.0,
                        y1: 10.0
                    }]
                },
                Hit {
                    page: 0,
                    areas: vec![PointRect {
                        x0: 0.0,
                        y0: 20.0,
                        x1: 20.0,
                        y1: 30.0
                    }]
                },
            ]
        );
    }
}
