use std::io::{BufRead, BufReader};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;

use crate::layout::Position;

const FRIEND_LISTS: u32 = 1024;
const SCALED_POINTS_PER_BIG_POINT: f64 = 65781.76;
const SEARCH_TRIES: usize = 100;
const VISIBLE_BOUND: f64 = 1_500_000.0;
const FAR: f64 = 2_147_483_647.0;
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLocation {
    pub file: PathBuf,
    pub line: u32,
}

#[derive(Debug)]
pub struct Synctex {
    inputs: Vec<(u32, PathBuf)>,
    nodes: Vec<Node>,
    hboxes: Vec<HBoxDetail>,
    pages: Vec<Option<Page>>,
    friends: Vec<Vec<Link>>,
    last_lines: Vec<u32>,
    scale: Scale,
    visible_bound: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Sheet,
    VBox,
    HBox,
    VoidVBox,
    VoidHBox,
    Kern,
    Glue,
    Rule,
    Math,
    Boundary,
    BoxEdge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Origin {
    tag: u32,
    line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Link(u32);

#[derive(Debug, Clone, Copy, Default)]
struct Extent {
    h: i32,
    v: i32,
    width: i32,
    height: i32,
    depth: i32,
}

#[derive(Debug)]
struct Node {
    kind: Kind,
    origin: Origin,
    at: Extent,
    parent: Link,
    first_child: Link,
    next_sibling: Link,
    detail: Link,
    listed_under: Option<u16>,
}

#[derive(Debug)]
struct HBoxDetail {
    seen: Extent,
    mean_line: i64,
    weight: i64,
}

#[derive(Debug)]
struct Page {
    sheet: usize,
    hboxes: Vec<Link>,
}

#[derive(Debug, Clone, Copy)]
struct Scale {
    unit: f64,
    x_offset: f64,
    y_offset: f64,
}

#[derive(Debug, Clone, Copy)]
struct Hit {
    h: f64,
    v: f64,
}

#[derive(Debug, Clone, Copy)]
struct Near {
    node: Option<usize>,
    distance: f64,
}

#[derive(Debug, Clone, Copy)]
struct Bounds {
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
}

impl Link {
    const NONE: Link = Link(u32::MAX);

    fn to(id: usize) -> Result<Self> {
        u32::try_from(id)
            .ok()
            .filter(|&id| id != u32::MAX)
            .map(Link)
            .context("too many SyncTeX records")
    }

    fn get(self) -> Option<usize> {
        (self != Link::NONE)
            .then(|| usize::try_from(self.0).ok())
            .flatten()
    }
}

impl Near {
    const NONE: Near = Near {
        node: None,
        distance: FAR,
    };
}

impl Extent {
    fn bounds(self) -> Bounds {
        let (h, width) = (f64::from(self.h), f64::from(self.width));
        let (left, right) = if width < 0.0 {
            (h + width, h)
        } else {
            (h, h + width)
        };
        Bounds {
            left,
            right,
            top: f64::from(self.v) - f64::from(self.height),
            bottom: f64::from(self.v) + f64::from(self.depth),
        }
    }

    fn span(self) -> Bounds {
        let (h, v) = (f64::from(self.h), f64::from(self.v));
        Bounds {
            left: h,
            right: h + f64::from(self.width).abs(),
            top: v - f64::from(self.height).abs(),
            bottom: v + f64::from(self.depth).abs(),
        }
    }
}

impl Synctex {
    pub fn beside(pdf: &Path) -> Option<PathBuf> {
        let stem = pdf.file_stem()?;
        let directory = pdf.parent().unwrap_or_else(|| Path::new(""));
        ["synctex.gz", "synctex"]
            .into_iter()
            .map(|extension| {
                let mut name = stem.to_os_string();
                name.push(".");
                name.push(extension);
                directory.join(name)
            })
            .find(|candidate| candidate.is_file())
    }

    pub fn open(path: &Path) -> Result<Self> {
        let file =
            std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
        let mut reader = BufReader::new(file);
        let compressed = reader
            .fill_buf()
            .with_context(|| format!("reading {}", path.display()))?
            .starts_with(&GZIP_MAGIC);
        let base = path.parent().unwrap_or_else(|| Path::new(""));
        let parsed = if compressed {
            Self::parse(BufReader::new(GzDecoder::new(reader)), base)
        } else {
            Self::parse(reader, base)
        };
        parsed.with_context(|| format!("parsing {}", path.display()))
    }

    pub fn source_at(&self, point: Position) -> Option<SourceLocation> {
        let page = self.pages.get(point.page + 1)?.as_ref()?;
        let hit = Hit {
            h: ((point.x - self.scale.x_offset) / self.scale.unit).trunc(),
            v: ((point.y - self.scale.y_offset) / self.scale.unit).trunc(),
        };
        let chosen = self.edit_query(page, hit)?;
        let origin = self.nodes[chosen].origin;
        let file = self.path_of(origin.tag)?;
        Some(SourceLocation {
            file,
            line: origin.line,
        })
    }

    pub fn position_of(&self, file: &Path, line: u32) -> Option<Position> {
        let tag = self.tag_of(file)?;
        let found = self.display_query(tag, line)?;
        let page = found.iter().filter_map(|&id| self.page_of(id)).min()?;
        let (top, left) = found
            .iter()
            .filter(|&&id| self.page_of(id) == Some(page))
            .map(|&id| self.visible_bounds(id))
            .map(|bounds| (bounds.top, bounds.left))
            .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)))?;
        Some(Position {
            page: page.checked_sub(1)?,
            x: left * self.scale.unit + self.scale.x_offset,
            y: top * self.scale.unit + self.scale.y_offset,
        })
    }

    fn parse(mut reader: impl BufRead, base: &Path) -> Result<Self> {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if !line.starts_with("SyncTeX Version:") {
            bail!("not a SyncTeX file");
        }
        let mut parser = Parser::new(base);
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            parser.line(line.trim_end_matches(['\n', '\r']))?;
        }
        Ok(parser.finish())
    }

    pub fn inputs(&self) -> impl Iterator<Item = &Path> {
        self.inputs.iter().map(|(_, path)| path.as_path())
    }

    fn path_of(&self, tag: u32) -> Option<PathBuf> {
        self.inputs
            .iter()
            .find(|(input, _)| *input == tag)
            .map(|(_, path)| path.clone())
    }

    fn tag_of(&self, file: &Path) -> Option<u32> {
        let wanted = without_dots(file);
        if let Some((tag, _)) = self.inputs.iter().find(|(_, path)| *path == wanted) {
            return Some(*tag);
        }
        let mut candidates = self
            .inputs
            .iter()
            .filter(|(_, path)| path.ends_with(&wanted) || wanted.ends_with(path));
        let (tag, path) = candidates.next()?;
        candidates.all(|(_, other)| other == path).then_some(*tag)
    }

    fn page_of(&self, id: usize) -> Option<usize> {
        let mut node = id;
        while let Some(parent) = self.nodes[node].parent.get() {
            node = parent;
        }
        self.pages
            .iter()
            .position(|page| page.as_ref().is_some_and(|page| page.sheet == node))
    }

    fn children(&self, id: usize) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(self.nodes[id].first_child.get(), |&child| {
            self.nodes[child].next_sibling.get()
        })
    }

    fn has_children(&self, id: usize) -> bool {
        self.nodes[id].first_child != Link::NONE
    }

    fn seen(&self, id: usize) -> Extent {
        let node = &self.nodes[id];
        node.detail
            .get()
            .map_or(node.at, |detail| self.hboxes[detail].seen)
    }

    fn display_query(&self, tag: u32, line: u32) -> Option<Vec<usize>> {
        let last = self
            .last_lines
            .get(usize::try_from(tag).ok()?)
            .copied()
            .unwrap_or(0);
        let mut line = i64::from(line.min(last));
        let last = i64::from(last);
        let mut step: i64 = 1;
        for _ in 0..SEARCH_TRIES {
            if line > last {
                break;
            }
            if let Ok(wanted) = u32::try_from(line) {
                let found = self
                    .friends_of(tag, wanted, true)
                    .or_else(|| self.friends_of(tag, wanted, false));
                if found.is_some() {
                    return found;
                }
            }
            line += step;
            step = next_step(step);
            if line <= 0 {
                line += step;
                step = next_step(step);
            }
        }
        None
    }

    fn friends_of(&self, tag: u32, line: u32, leaves_only: bool) -> Option<Vec<usize>> {
        let list = friend_list(tag, line);
        let found: Vec<usize> = self.friends[usize::from(list)]
            .iter()
            .filter_map(|link| link.get())
            .filter(|&id| {
                let node = &self.nodes[id];
                node.origin == Origin { tag, line }
                    && node.listed_under == Some(list)
                    && !(leaves_only && node.kind.is_box())
            })
            .collect();
        (!found.is_empty()).then_some(found)
    }

    fn visible_bounds(&self, id: usize) -> Bounds {
        self.seen(self.visible_box(id)).bounds()
    }

    fn visible_box(&self, id: usize) -> usize {
        let mut node = if self.nodes[id].kind.is_box() {
            id
        } else {
            self.nodes[id].parent.get().unwrap_or(id)
        };
        let mean = self.mean_line(node);
        let mut above = self.nodes[node].parent.get();
        while let Some(parent) = above {
            let candidate = &self.nodes[parent];
            if let Some(detail) = candidate.detail.get() {
                if (mean - self.hboxes[detail].mean_line).abs() > 1 {
                    return node;
                }
                let at = candidate.at;
                if f64::from(at.width) > self.visible_bound
                    || f64::from(at.height) + f64::from(at.depth) > self.visible_bound
                {
                    return parent;
                }
                node = parent;
            }
            above = candidate.parent.get();
        }
        node
    }

    fn mean_line(&self, id: usize) -> i64 {
        let node = &self.nodes[id];
        if let Some(detail) = node.detail.get() {
            return self.hboxes[detail].mean_line;
        }
        node.parent
            .get()
            .and_then(|parent| self.nodes[parent].detail.get())
            .map_or(i64::from(node.origin.line), |detail| {
                self.hboxes[detail].mean_line
            })
    }

    fn edit_query(&self, page: &Page, hit: Hit) -> Option<usize> {
        let mut order = page.hboxes.iter().rev().filter_map(|link| link.get());
        let (node, (left, right)) = loop {
            let Some(hbox) = order.next() else {
                let first = self.nodes[page.sheet].first_child.get()?;
                let near = self.closest_deep_child(hit, first);
                break (Some(first), (near, Near::NONE));
            };
            if self.contains(hit, hbox) {
                let smallest = order
                    .filter(|&next| self.contains(hit, next))
                    .fold(hbox, |node, next| self.smaller(next, node));
                let deepest = self.deepest_container(hit, smallest);
                let near = deepest.map_or((Near::NONE, Near::NONE), |deepest| {
                    self.closest_children(hit, deepest)
                });
                break (deepest, near);
            }
        };
        match (left.node, right.node) {
            (Some(l), Some(r)) => {
                let (lo, ro) = (self.nodes[l].origin, self.nodes[r].origin);
                let right_first = if lo == ro {
                    left.distance > right.distance
                } else {
                    ro.line < lo.line || (ro.line == lo.line && left.distance > right.distance)
                };
                Some(if right_first { r } else { l })
            }
            (None, Some(r)) => Some(r),
            (Some(l), None) => Some(l),
            (None, None) => node,
        }
    }

    fn deepest_container(&self, hit: Hit, id: usize) -> Option<usize> {
        if !self.has_children(id) {
            return None;
        }
        for child in self.children(id) {
            if self.contains(hit, child)
                && let Some(deep) = self.deepest_container(hit, child)
            {
                return Some(deep);
            }
        }
        if self.nodes[id].kind == Kind::VBox {
            let best = self.closest_filled_child(hit, id, |distance, best| distance <= best);
            if best.node.is_some() {
                return best.node;
            }
        }
        self.contains(hit, id).then_some(id)
    }

    fn deepest_container_near(&self, hit: Hit, id: usize) -> Option<Near> {
        if !self.has_children(id) {
            return None;
        }
        for child in self.children(id) {
            if let Some(deep) = self.deepest_container_near(hit, child) {
                return Some(deep);
            }
        }
        if self.nodes[id].kind == Kind::VBox {
            let best = self.closest_filled_child(hit, id, |distance, best| distance < best);
            if best.node.is_some() {
                return Some(best);
            }
        }
        self.contains(hit, id).then_some(Near {
            node: Some(id),
            distance: 0.0,
        })
    }

    fn closest_filled_child(&self, hit: Hit, id: usize, better: impl Fn(f64, f64) -> bool) -> Near {
        let mut best = Near::NONE;
        for child in self.children(id).filter(|&child| self.has_children(child)) {
            let distance = self.distance(hit, child);
            if better(distance, best.distance) {
                best = Near {
                    node: Some(child),
                    distance,
                };
            }
        }
        best
    }

    fn closest_deep_child(&self, hit: Hit, id: usize) -> Near {
        let mut best = Near::NONE;
        for child in self.children(id) {
            let near = if self.nodes[child].kind.is_box() {
                self.closest_deep_child(hit, child)
            } else {
                Near {
                    node: Some(child),
                    distance: self.distance(hit, child),
                }
            };
            let is_kern = near
                .node
                .is_some_and(|node| self.nodes[node].kind == Kind::Kern);
            if near.distance < best.distance || (near.distance == best.distance && !is_kern) {
                best = near;
            }
        }
        best
    }

    fn closest_children(&self, hit: Hit, id: usize) -> (Near, Near) {
        if !self.has_children(id) || self.nodes[id].kind != Kind::HBox {
            return (Near::NONE, Near::NONE);
        }
        let mut left = Near::NONE;
        let mut right = Near::NONE;
        for child in self.children(id) {
            let distance = self.across(hit, child);
            let near = Near {
                node: Some(child),
                distance: distance.abs(),
            };
            if distance > 0.0 {
                if right.distance > distance || self.earlier_line(right, near) {
                    right = near;
                }
            } else if distance == 0.0 {
                if self.has_children(child) {
                    return self.closest_children(hit, child);
                }
                left = near;
            } else if left.distance > near.distance || self.earlier_line(left, near) {
                left = near;
            }
        }
        (self.narrow(hit, left), self.narrow(hit, right))
    }

    fn earlier_line(&self, kept: Near, candidate: Near) -> bool {
        let (Some(kept_node), Some(candidate_node)) = (kept.node, candidate.node) else {
            return false;
        };
        let (kept_origin, candidate_origin) = (
            self.nodes[kept_node].origin,
            self.nodes[candidate_node].origin,
        );
        kept.distance == candidate.distance
            && kept_origin.tag == candidate_origin.tag
            && kept_origin.line > candidate_origin.line
    }

    fn narrow(&self, hit: Hit, near: Near) -> Near {
        let Some(node) = near.node else {
            return near;
        };
        let mut near = self.deepest_container_near(hit, node).unwrap_or(near);
        if let Some(inner) = near
            .node
            .and_then(|node| self.closest_deep_child(hit, node).node)
        {
            near.node = Some(inner);
        }
        near
    }

    fn smaller(&self, node: usize, other: usize) -> usize {
        let height = |id: usize| {
            let seen = self.seen(id);
            f64::from(seen.depth).abs() + f64::from(seen.height).abs()
        };
        let area = |id: usize| height(id) * f64::from(self.seen(id).width).abs();
        let width = |id: usize| f64::from(self.nodes[id].at.width).abs();
        match area(node).total_cmp(&area(other)) {
            std::cmp::Ordering::Less => return node,
            std::cmp::Ordering::Greater => return other,
            std::cmp::Ordering::Equal => {}
        }
        match width(node).total_cmp(&width(other)) {
            std::cmp::Ordering::Greater => return node,
            std::cmp::Ordering::Less => return other,
            std::cmp::Ordering::Equal => {}
        }
        if height(node) > height(other) {
            other
        } else {
            node
        }
    }

    fn contains(&self, hit: Hit, id: usize) -> bool {
        self.across(hit, id) == 0.0 && self.down(hit, id) == 0.0
    }

    fn across(&self, hit: Hit, id: usize) -> f64 {
        let node = &self.nodes[id];
        let h = f64::from(node.at.h);
        let width = f64::from(node.at.width);
        match node.kind {
            Kind::VBox | Kind::VoidVBox | Kind::VoidHBox | Kind::HBox => {
                let span = self.seen(id).span();
                signed_gap(hit.h, span.left, span.right)
            }
            Kind::Kern => {
                let (min, max) = if width < 0.0 {
                    (h, h - width)
                } else {
                    (h - width, h)
                };
                let middle = ((min + max) / 2.0).trunc();
                if hit.h < min {
                    min - hit.h + 1.0
                } else if hit.h > max {
                    max - hit.h - 1.0
                } else if hit.h > middle {
                    max - hit.h + 1.0
                } else {
                    min - hit.h - 1.0
                }
            }
            Kind::Rule | Kind::Glue | Kind::Math | Kind::Boundary | Kind::BoxEdge => h - hit.h,
            Kind::Sheet => FAR,
        }
    }

    fn down(&self, hit: Hit, id: usize) -> f64 {
        let node = &self.nodes[id];
        match node.kind {
            Kind::VBox | Kind::VoidVBox | Kind::VoidHBox | Kind::HBox => {
                let span = self.seen(id).span();
                signed_gap(hit.v, span.top, span.bottom)
            }
            Kind::Rule | Kind::Kern | Kind::Glue | Kind::Math => {
                let span = self.leaf_span(id, f64::from(node.at.h));
                signed_gap(hit.v, span.top, span.bottom)
            }
            Kind::Boundary | Kind::BoxEdge | Kind::Sheet => FAR,
        }
    }

    fn distance(&self, hit: Hit, id: usize) -> f64 {
        let node = &self.nodes[id];
        let at = node.at;
        let h = f64::from(at.h);
        let edge = |h: f64| Bounds {
            bottom: f64::from(at.v),
            ..self.leaf_span(id, h)
        };
        match node.kind {
            Kind::VBox | Kind::HBox => box_distance(hit, self.seen(id).span()),
            Kind::VoidVBox | Kind::VoidHBox => {
                let span = at.span();
                let side = |h: f64| Bounds {
                    left: h,
                    right: h,
                    ..span
                };
                box_distance(hit, side(span.left)).min(box_distance(hit, side(span.right)))
            }
            Kind::Kern => {
                box_distance(hit, edge(h)).min(box_distance(hit, edge(h - f64::from(at.width))))
            }
            Kind::Glue | Kind::Math | Kind::Boundary | Kind::BoxEdge => box_distance(hit, edge(h)),
            Kind::Rule | Kind::Sheet => FAR,
        }
    }

    fn leaf_span(&self, id: usize, h: f64) -> Bounds {
        let node = &self.nodes[id];
        let parent = node
            .parent
            .get()
            .map(|parent| self.nodes[parent].at)
            .unwrap_or_default();
        let v = f64::from(node.at.v);
        Bounds {
            left: h,
            right: h,
            top: v - f64::from(parent.height).abs(),
            bottom: v + f64::from(parent.depth).abs(),
        }
    }
}

impl Kind {
    fn is_box(self) -> bool {
        matches!(
            self,
            Kind::VBox | Kind::HBox | Kind::VoidVBox | Kind::VoidHBox
        )
    }
}

struct Frame {
    node: usize,
    last_child: Option<usize>,
    waiting: Vec<usize>,
}

struct Parser<'a> {
    base: &'a Path,
    inputs: Vec<(u32, PathBuf)>,
    nodes: Vec<Node>,
    hboxes: Vec<HBoxDetail>,
    pages: Vec<Option<Page>>,
    friends: Vec<Vec<Link>>,
    last_lines: Vec<u32>,
    unit: f64,
    magnification: f64,
    x_offset: f64,
    y_offset: f64,
    page: usize,
    frames: Vec<Frame>,
    last_kern: Option<usize>,
    last_glue: Option<usize>,
    last_v: i32,
}

struct Record {
    origin: Origin,
    at: Extent,
}

impl<'a> Parser<'a> {
    fn new(base: &'a Path) -> Self {
        Self {
            base,
            inputs: Vec::new(),
            nodes: Vec::new(),
            hboxes: Vec::new(),
            pages: Vec::new(),
            friends: (0..FRIEND_LISTS).map(|_| Vec::new()).collect(),
            last_lines: Vec::new(),
            unit: 1.0,
            magnification: 1000.0,
            x_offset: 0.0,
            y_offset: 0.0,
            page: 0,
            frames: Vec::new(),
            last_kern: None,
            last_glue: None,
            last_v: -1,
        }
    }

    fn finish(self) -> Synctex {
        let unit_scale = self.unit / SCALED_POINTS_PER_BIG_POINT;
        Synctex {
            inputs: self.inputs,
            nodes: self.nodes,
            hboxes: self.hboxes,
            pages: self.pages,
            friends: self.friends,
            last_lines: self.last_lines,
            scale: Scale {
                unit: unit_scale * self.magnification / 1000.0,
                x_offset: self.x_offset * unit_scale,
                y_offset: self.y_offset * unit_scale,
            },
            visible_bound: VISIBLE_BOUND / (self.magnification / 1000.0),
        }
    }

    fn line(&mut self, line: &str) -> Result<()> {
        let Some(kind) = line.chars().next() else {
            return Ok(());
        };
        let body = &line[kind.len_utf8()..];
        match kind {
            'x' => self.boundary(body)?,
            'k' => self.kern(body)?,
            'g' => self.glue(body)?,
            '(' => self.open_box(Kind::HBox, body)?,
            ')' => self.close_hbox()?,
            '[' => self.open_box(Kind::VBox, body)?,
            ']' => self.close_vbox(),
            'h' => self.void_box(Kind::VoidHBox, body)?,
            'v' => self.void_box(Kind::VoidVBox, body)?,
            'r' => self.rule(body)?,
            '$' => self.math(body)?,
            'c' => self.forget_kern(),
            '{' => self.open_page(body)?,
            '}' => self.frames.clear(),
            'I' if line.starts_with("Input:") => self.input(&line[6..])?,
            'U' if line.starts_with("Unit:") => self.unit = decimal(&line[5..])?,
            'M' if line.starts_with("Magnification:") => {
                self.magnification = decimal(&line[14..])?;
            }
            'X' if line.starts_with("X Offset:") => self.x_offset = decimal(&line[9..])?,
            'Y' if line.starts_with("Y Offset:") => self.y_offset = decimal(&line[9..])?,
            _ => {}
        }
        Ok(())
    }

    fn input(&mut self, rest: &str) -> Result<()> {
        let (tag, path) = rest
            .split_once(':')
            .context("an Input line without a path")?;
        let tag = tag.parse().context("an Input line without a tag")?;
        self.inputs.push((tag, without_dots(&self.base.join(path))));
        Ok(())
    }

    fn open_page(&mut self, body: &str) -> Result<()> {
        let page: usize = body.trim().parse().context("a page without a number")?;
        self.page = page;
        self.frames.clear();
        let sheet = self.nodes.len();
        self.nodes.push(Node::new(
            Kind::Sheet,
            Origin { tag: 0, line: 0 },
            Extent::default(),
        ));
        if self.pages.len() <= page {
            self.pages.resize_with(page + 1, || None);
        }
        self.pages[page] = Some(Page {
            sheet,
            hboxes: Vec::new(),
        });
        self.frames.push(Frame {
            node: sheet,
            last_child: None,
            waiting: Vec::new(),
        });
        Ok(())
    }

    fn open_box(&mut self, kind: Kind, body: &str) -> Result<()> {
        let record = self.record(body)?;
        let id = self.append(kind, &record)?;
        self.register(record.origin);
        self.frames.push(Frame {
            node: id,
            last_child: None,
            waiting: Vec::new(),
        });
        if kind == Kind::HBox {
            self.nodes[id].detail = Link::to(self.hboxes.len())?;
            self.hboxes.push(HBoxDetail {
                seen: record.at,
                mean_line: i64::from(record.origin.line),
                weight: 1,
            });
            let edge = Record {
                origin: record.origin,
                at: Extent {
                    h: record.at.h,
                    v: record.at.v,
                    ..Extent::default()
                },
            };
            let edge = self.append(Kind::BoxEdge, &edge)?;
            self.befriend(edge);
        }
        self.forget_kern();
        Ok(())
    }

    fn close_vbox(&mut self) {
        let Some(frame) = self.frames.last() else {
            return;
        };
        let id = frame.node;
        if self.nodes[id].kind != Kind::VBox {
            return;
        }
        if self.nodes[id].first_child == Link::NONE {
            self.befriend(id);
        }
        self.leave(id);
        self.forget_kern();
    }

    fn close_hbox(&mut self) -> Result<()> {
        let Some(frame) = self.frames.last() else {
            return Ok(());
        };
        let (id, last_child) = (frame.node, frame.last_child);
        let Some(detail) = self.nodes[id].detail.get() else {
            return Ok(());
        };
        if let Some(Some(page)) = self.pages.get_mut(self.page) {
            page.hboxes.push(Link::to(id)?);
        }
        let first = self.nodes[id].first_child.get();
        let second = first.and_then(|first| self.nodes[first].next_sibling.get());
        if let (Some(first), Some(second)) = (first, second) {
            self.nodes[first].origin.line = self.nodes[second].origin.line;
            let (mut weight, mut total) = (0, 0);
            let mut child = Some(second);
            while let Some(current) = child {
                let node = &self.nodes[current];
                if let Some(inner) = node.detail.get() {
                    let inner = &self.hboxes[inner];
                    weight += inner.weight;
                    total += inner.mean_line * inner.weight;
                } else {
                    weight += 1;
                    total += i64::from(node.origin.line);
                }
                child = node.next_sibling.get();
            }
            self.hboxes[detail].mean_line = (total + weight / 2) / weight;
            self.hboxes[detail].weight = weight;
        }
        let seen = self.hboxes[detail].seen;
        let end = Record {
            origin: last_child.map_or(self.nodes[id].origin, |last| self.nodes[last].origin),
            at: Extent {
                h: seen.h.saturating_add(seen.width),
                v: seen.v,
                ..Extent::default()
            },
        };
        self.append(Kind::BoxEdge, &end)?;
        if let Some(first) = first {
            self.nodes[first].at.h = seen.h;
            self.nodes[first].at.v = seen.v;
        }
        if let (Some(kern), Some(glue)) = (self.last_kern, self.last_glue) {
            let mut child = first;
            while let Some(current) = child {
                let next = self.nodes[current].next_sibling.get();
                if next == Some(kern) {
                    let origin = self.nodes[current].origin;
                    self.nodes[kern].origin = origin;
                    self.nodes[glue].origin = origin;
                    break;
                }
                child = next;
            }
        }
        self.leave(id);
        self.stretch_to(seen.bounds());
        self.forget_kern();
        Ok(())
    }

    fn leave(&mut self, id: usize) {
        if let Some(frame) = self.frames.pop() {
            for waiting in frame.waiting {
                self.befriend(waiting);
            }
        }
        self.settle_waiting(id);
    }

    fn void_box(&mut self, kind: Kind, body: &str) -> Result<()> {
        let record = self.record(body)?;
        let id = self.append(kind, &record)?;
        self.settle_waiting(id);
        if kind == Kind::VoidHBox {
            self.stretch_to(record.at.bounds());
        }
        self.register(record.origin);
        self.forget_kern();
        Ok(())
    }

    fn kern(&mut self, body: &str) -> Result<()> {
        let record = self.record(body)?;
        let id = self.leaf(Kind::Kern, &record)?;
        let (h, width, v) = (
            f64::from(record.at.h),
            f64::from(record.at.width),
            f64::from(record.at.v),
        );
        let (left, right) = if width > 0.0 {
            (h - width, h)
        } else {
            (h, h - width)
        };
        self.stretch_to(Bounds {
            left,
            right,
            top: v,
            bottom: v,
        });
        self.last_kern = Some(id);
        self.last_glue = None;
        Ok(())
    }

    fn glue(&mut self, body: &str) -> Result<()> {
        let record = self.record(body)?;
        let id = self.leaf(Kind::Glue, &record)?;
        self.stretch_to_point(record.at);
        if self.last_kern.is_some() {
            self.last_glue = Some(id);
        } else {
            self.forget_kern();
        }
        Ok(())
    }

    fn rule(&mut self, body: &str) -> Result<()> {
        let record = self.record(body)?;
        self.leaf(Kind::Rule, &record)?;
        self.forget_kern();
        Ok(())
    }

    fn math(&mut self, body: &str) -> Result<()> {
        let record = self.record(body)?;
        self.leaf(Kind::Math, &record)?;
        self.stretch_to_point(record.at);
        self.forget_kern();
        Ok(())
    }

    fn leaf(&mut self, kind: Kind, record: &Record) -> Result<usize> {
        let id = self.append(kind, record)?;
        self.befriend(id);
        self.settle_waiting(id);
        self.register(record.origin);
        Ok(id)
    }

    fn boundary(&mut self, body: &str) -> Result<()> {
        let record = self.record(body)?;
        let Some(frame) = self.frames.last() else {
            return Ok(());
        };
        let leads = frame
            .last_child
            .is_some_and(|previous| self.nodes[previous].kind == Kind::BoxEdge)
            || !frame.waiting.is_empty();
        let id = self.append(Kind::Boundary, &record)?;
        if leads {
            if let Some(frame) = self.frames.last_mut() {
                frame.waiting.push(id);
            }
        } else {
            self.befriend(id);
        }
        self.stretch_to_point(record.at);
        self.register(record.origin);
        self.forget_kern();
        Ok(())
    }

    fn forget_kern(&mut self) {
        self.last_kern = None;
        self.last_glue = None;
    }

    fn settle_waiting(&mut self, child: usize) {
        let Some(frame) = self.frames.last_mut() else {
            return;
        };
        if frame.waiting.is_empty() {
            return;
        }
        let waiting = std::mem::take(&mut frame.waiting);
        let origin = self.nodes[child].origin;
        for id in waiting {
            self.nodes[id].origin = origin;
            self.befriend(id);
        }
    }

    fn befriend(&mut self, id: usize) {
        let origin = self.nodes[id].origin;
        let list = friend_list(origin.tag, origin.line);
        self.nodes[id].listed_under = Some(list);
        if let Ok(link) = Link::to(id) {
            self.friends[usize::from(list)].push(link);
        }
    }

    fn register(&mut self, origin: Origin) {
        let Ok(tag) = usize::try_from(origin.tag) else {
            return;
        };
        if self.last_lines.len() <= tag {
            self.last_lines.resize(tag + 1, 0);
        }
        self.last_lines[tag] = self.last_lines[tag].max(origin.line);
    }

    fn append(&mut self, kind: Kind, record: &Record) -> Result<usize> {
        let frame = self.frames.last_mut().context("a record outside a page")?;
        let id = self.nodes.len();
        let link = Link::to(id)?;
        let mut node = Node::new(kind, record.origin, record.at);
        node.parent = Link::to(frame.node)?;
        match frame.last_child.replace(id) {
            Some(previous) => self.nodes[previous].next_sibling = link,
            None => self.nodes[frame.node].first_child = link,
        }
        self.nodes.push(node);
        Ok(id)
    }

    fn stretch_to_point(&mut self, at: Extent) {
        let (h, v) = (f64::from(at.h), f64::from(at.v));
        self.stretch_to(Bounds {
            left: h,
            right: h,
            top: v,
            bottom: v,
        });
    }

    fn stretch_to(&mut self, bounds: Bounds) {
        let Some(frame) = self.frames.last() else {
            return;
        };
        let Some(detail) = self.nodes[frame.node].detail.get() else {
            return;
        };
        let seen = &mut self.hboxes[detail].seen;
        let (h, width, v) = (f64::from(seen.h), f64::from(seen.width), f64::from(seen.v));
        if width < 0.0 {
            let (min, max) = (h + width, h);
            if bounds.left < min {
                seen.width = whole(bounds.left - max);
            } else if bounds.right > max {
                seen.h = whole(bounds.right);
                seen.width = whole(min - bounds.right);
            }
        } else {
            let (min, max) = (h, h + width);
            if bounds.left < min {
                seen.h = whole(bounds.left);
                seen.width = whole(max - bounds.left);
            } else if bounds.right > max {
                seen.width = whole(bounds.right - min);
            }
        }
        if bounds.top < v - f64::from(seen.height) {
            seen.height = whole(v - bounds.top);
        } else if bounds.bottom > v + f64::from(seen.depth) {
            seen.depth = whole(bounds.bottom - v);
        }
    }

    fn record(&mut self, body: &str) -> Result<Record> {
        let mut fields = Fields::new(body);
        let tag = fields.whole()?;
        fields.expect(b',')?;
        let line = fields.whole()?;
        fields.skip_past(b':')?;
        let h = fields.whole()?;
        fields.expect(b',')?;
        let v = if fields.take(b'=') {
            self.last_v
        } else {
            fields.whole()?
        };
        self.last_v = v;
        let mut size = [0; 3];
        if fields.take(b':') {
            for (index, value) in size.iter_mut().enumerate() {
                if index > 0 && !fields.take(b',') {
                    break;
                }
                *value = fields.whole()?;
            }
        }
        let [width, height, depth] = size;
        Ok(Record {
            origin: Origin {
                tag: u32::try_from(tag).context("a record with a negative tag")?,
                line: u32::try_from(line).context("a record with a negative line")?,
            },
            at: Extent {
                h,
                v,
                width,
                height,
                depth,
            },
        })
    }
}

impl Node {
    fn new(kind: Kind, origin: Origin, at: Extent) -> Self {
        Self {
            kind,
            origin,
            at,
            parent: Link::NONE,
            first_child: Link::NONE,
            next_sibling: Link::NONE,
            detail: Link::NONE,
            listed_under: None,
        }
    }
}

fn friend_list(tag: u32, line: u32) -> u16 {
    let list = tag.wrapping_add(line) % FRIEND_LISTS;
    u16::try_from(list).unwrap_or_default()
}

fn next_step(step: i64) -> i64 {
    if step < 0 { -(step - 1) } else { -(step + 1) }
}

fn signed_gap(value: f64, min: f64, max: f64) -> f64 {
    if value < min {
        min - value
    } else if value > max {
        max - value
    } else {
        0.0
    }
}

fn box_distance(hit: Hit, bounds: Bounds) -> f64 {
    let horizontal = if hit.h < bounds.left {
        bounds.left - hit.h
    } else if hit.h > bounds.right {
        hit.h - bounds.right
    } else {
        0.0
    };
    let vertical = if hit.v < bounds.top {
        bounds.top - hit.v
    } else if hit.v > bounds.bottom {
        hit.v - bounds.bottom
    } else {
        0.0
    };
    horizontal + vertical
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the value is a sum of two SyncTeX coordinates, which are whole numbers that fit in i32"
)]
fn whole(value: f64) -> i32 {
    value as i32
}

struct Fields<'a> {
    text: &'a str,
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Fields<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            bytes: text.as_bytes(),
            at: 0,
        }
    }

    fn take(&mut self, wanted: u8) -> bool {
        let found = self.bytes.get(self.at) == Some(&wanted);
        if found {
            self.at += 1;
        }
        found
    }

    fn expect(&mut self, wanted: u8) -> Result<()> {
        if self.take(wanted) {
            Ok(())
        } else {
            bail!("{:?} lacks a {:?}", self.text, char::from(wanted))
        }
    }

    fn skip_past(&mut self, wanted: u8) -> Result<()> {
        let offset = self.bytes[self.at..]
            .iter()
            .position(|&byte| byte == wanted)
            .with_context(|| format!("{:?} lacks a {:?}", self.text, char::from(wanted)))?;
        self.at += offset + 1;
        Ok(())
    }

    fn whole(&mut self) -> Result<i32> {
        let negative = self.take(b'-');
        let start = self.at;
        let mut value: i64 = 0;
        while let Some(digit) = self.bytes.get(self.at).filter(|byte| byte.is_ascii_digit()) {
            value = value * 10 + i64::from(digit - b'0');
            if value > i64::from(u32::MAX) {
                bail!("{:?} holds a number that is too large", self.text);
            }
            self.at += 1;
        }
        if self.at == start {
            bail!("{:?} lacks a number", self.text);
        }
        let value = if negative { -value } else { value };
        i32::try_from(value)
            .with_context(|| format!("{:?} holds a number that is too large", self.text))
    }
}

fn decimal(text: &str) -> Result<f64> {
    text.trim()
        .parse()
        .with_context(|| format!("{text:?} is not a number"))
}

fn without_dots(path: &Path) -> PathBuf {
    path.components()
        .filter(|part| *part != Component::CurDir)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    const GOLDEN: &str = include_str!("../tests/fixtures/synctex/thesis.golden");
    const TOLERANCE: f64 = 0.01;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/synctex")
    }

    fn thesis() -> Synctex {
        Synctex::open(&fixtures().join("thesis.synctex.gz")).expect("the thesis fixture parses")
    }

    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let directory = std::env::temp_dir().join(format!("termleaf-synctex-{name}-{nanos}"));
        std::fs::create_dir_all(&directory).expect("a scratch directory");
        directory
    }

    fn golden(kind: &str) -> impl Iterator<Item = Vec<&'static str>> {
        GOLDEN
            .lines()
            .map(|line| line.split(' ').collect::<Vec<_>>())
            .filter(move |fields| fields[0] == kind)
    }

    fn number(field: &str) -> f64 {
        field.parse().expect("a number in the golden file")
    }

    fn page(field: &str) -> usize {
        field.parse().expect("a page in the golden file")
    }

    #[test]
    fn every_forward_answer_matches_the_synctex_command() {
        let synctex = thesis();
        let mut checked = 0;
        for fields in golden("view") {
            let line: u32 = fields[2].parse().expect("a line in the golden file");
            let found = synctex
                .position_of(Path::new(fields[1]), line)
                .unwrap_or_else(|| panic!("{} line {line} has a position", fields[1]));
            let wanted = (page(fields[3]) - 1, number(fields[4]), number(fields[5]));
            assert_eq!(found.page, wanted.0, "{} line {line}", fields[1]);
            assert!(
                (found.x - wanted.1).abs() < TOLERANCE && (found.y - wanted.2).abs() < TOLERANCE,
                "{} line {line}: {found:?} against {wanted:?}",
                fields[1]
            );
            checked += 1;
        }
        assert_eq!(checked, 137);
    }

    #[test]
    fn every_inverse_answer_matches_the_synctex_command() {
        let synctex = thesis();
        let mut checked = 0;
        for fields in golden("edit") {
            let point = Position {
                page: page(fields[1]) - 1,
                x: number(fields[2]),
                y: number(fields[3]),
            };
            let line: u32 = fields[5].parse().expect("a line in the golden file");
            let found = synctex
                .source_at(point)
                .unwrap_or_else(|| panic!("{point:?} has a source"));
            assert!(
                found.file.ends_with(fields[4]) && found.line == line,
                "{point:?}: {found:?} against {} line {line}",
                fields[4]
            );
            checked += 1;
        }
        assert_eq!(checked, 1395);
    }

    #[test]
    fn a_file_outside_the_document_has_no_position() {
        assert_eq!(thesis().position_of(Path::new("appendix.tex"), 3), None);
    }

    #[test]
    fn a_point_on_a_page_the_document_lacks_has_no_source() {
        let point = Position {
            page: 40,
            x: 200.0,
            y: 300.0,
        };
        assert_eq!(thesis().source_at(point), None);
    }

    #[test]
    fn an_uncompressed_synctex_file_gives_the_same_answers() {
        let mut text = String::new();
        GzDecoder::new(
            std::fs::read(fixtures().join("thesis.synctex.gz"))
                .expect("the fixture")
                .as_slice(),
        )
        .read_to_string(&mut text)
        .expect("the fixture unpacks");
        let directory = scratch("plain");
        let plain = directory.join("thesis.synctex");
        std::fs::write(&plain, text).expect("the plain copy");
        let synctex = Synctex::open(&plain).expect("the plain copy parses");
        let file = Path::new("chapters/method.tex");
        assert_eq!(
            synctex.position_of(file, 30),
            thesis().position_of(file, 30)
        );
        std::fs::remove_dir_all(directory).expect("the scratch directory is removed");
    }

    #[test]
    fn a_synctex_file_cut_short_is_an_error() {
        let bytes = std::fs::read(fixtures().join("thesis.synctex.gz")).expect("the fixture");
        let directory = scratch("cut");
        let cut = directory.join("thesis.synctex.gz");
        std::fs::write(&cut, &bytes[..bytes.len() / 2]).expect("the cut copy");
        assert!(Synctex::open(&cut).is_err());
        std::fs::remove_dir_all(directory).expect("the scratch directory is removed");
    }

    #[test]
    fn a_file_that_is_not_synctex_is_refused() {
        let error = Synctex::parse("%PDF-1.5\n".as_bytes(), Path::new("")).expect_err("a refusal");
        assert_eq!(error.to_string(), "not a SyncTeX file");
    }

    #[test]
    fn a_pdf_built_without_synctex_has_no_synctex_file() {
        let pdf = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/three-pages.pdf");
        assert_eq!(Synctex::beside(&pdf), None);
    }

    #[test]
    fn relative_inputs_resolve_against_the_synctex_directory() {
        let text = "SyncTeX Version:1\nInput:1:./main.tex\nOutput:pdf\nMagnification:1000\nUnit:1\nX Offset:0\nY Offset:0\nContent:\n{1\n[1,1:4736286,4736286:30000000,40000000,0\n(1,3:4736286,6000000:26000000,500000,100000\nx1,3:5000000,6000000\ng1,3:9000000,6000000\n)\n]\n}1\n";
        let synctex =
            Synctex::parse(text.as_bytes(), Path::new("/project")).expect("the sample parses");
        let point = Position {
            page: 0,
            x: 100.0,
            y: 90.0,
        };
        assert_eq!(
            synctex.source_at(point),
            Some(SourceLocation {
                file: PathBuf::from("/project/main.tex"),
                line: 3,
            })
        );
        let found = synctex
            .position_of(Path::new("/project/main.tex"), 3)
            .expect("a position");
        assert_eq!(found.page, 0);
        assert!((found.x - 4_736_286.0 / SCALED_POINTS_PER_BIG_POINT).abs() < TOLERANCE);
    }
}
