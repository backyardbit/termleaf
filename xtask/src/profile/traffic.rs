use std::ops::AddAssign;

const KITTY_START: &[u8] = b"\x1b_G";
const DCS_START: &[u8] = b"\x1bP";
const ITERM2_START: &[u8] = b"\x1b]1337;File";
const STRING_TERMINATOR: &[u8] = b"\x1b\\";
const BELL: u8 = 0x07;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Traffic {
    pub bytes: u64,
    pub graphics_bytes: u64,
    pub transmissions: u64,
    pub transmitted_pixels: u64,
    pub out_of_band_transmissions: u64,
    pub placements: u64,
    pub deletions: u64,
    pub sixel_frames: u64,
    pub sixel_pixels: u64,
    pub iterm2_images: u64,
}

impl AddAssign for Traffic {
    fn add_assign(&mut self, other: Self) {
        self.bytes += other.bytes;
        self.graphics_bytes += other.graphics_bytes;
        self.transmissions += other.transmissions;
        self.transmitted_pixels += other.transmitted_pixels;
        self.out_of_band_transmissions += other.out_of_band_transmissions;
        self.placements += other.placements;
        self.deletions += other.deletions;
        self.sixel_frames += other.sixel_frames;
        self.sixel_pixels += other.sixel_pixels;
        self.iterm2_images += other.iterm2_images;
    }
}

impl Traffic {
    pub fn images_sent(&self) -> u64 {
        self.transmissions + self.sixel_frames + self.iterm2_images
    }

    pub fn pixels_sent(&self) -> u64 {
        self.transmitted_pixels + self.sixel_pixels
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub at: usize,
    pub traffic: Traffic,
}

pub fn scan(stream: &[u8]) -> Vec<Mark> {
    let mut marks = Vec::new();
    let mut continuing = false;
    let mut offset = 0;
    while let Some((found, kind)) = next_sequence(&stream[offset..]) {
        let at = offset + found;
        let Some(length) = sequence_length(&stream[at..], kind) else {
            break;
        };
        let body = &stream[at..at + length];
        let mut traffic = Traffic {
            graphics_bytes: u64_from(length),
            ..Traffic::default()
        };
        match kind {
            Sequence::Kitty => kitty(body, &mut continuing, &mut traffic),
            Sequence::Dcs => sixel(body, &mut traffic),
            Sequence::Iterm2 => traffic.iterm2_images += 1,
        }
        marks.push(Mark { at, traffic });
        offset = at + length;
    }
    marks
}

pub fn between(marks: &[Mark], range: std::ops::Range<usize>) -> Traffic {
    let mut traffic = Traffic {
        bytes: u64_from(range.end.saturating_sub(range.start)),
        ..Traffic::default()
    };
    for mark in marks.iter().filter(|mark| range.contains(&mark.at)) {
        traffic += Traffic {
            bytes: 0,
            ..mark.traffic
        };
    }
    traffic
}

fn kitty(body: &[u8], continuing: &mut bool, traffic: &mut Traffic) {
    let control = kitty_control(body);
    let continuation = *continuing;
    *continuing = control.more_chunks;
    if continuation {
        return;
    }
    match control.action {
        b't' | b'T' | b'f' => {
            traffic.transmissions += 1;
            traffic.transmitted_pixels += control.width * control.height;
            if control.medium != b'd' {
                traffic.out_of_band_transmissions += 1;
            }
        }
        b'p' => traffic.placements += 1,
        b'd' => traffic.deletions += 1,
        _ => {}
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sequence {
    Kitty,
    Dcs,
    Iterm2,
}

fn next_sequence(stream: &[u8]) -> Option<(usize, Sequence)> {
    stream
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == 0x1b)
        .find_map(|(at, _)| {
            let rest = &stream[at..];
            [
                (KITTY_START, Sequence::Kitty),
                (DCS_START, Sequence::Dcs),
                (ITERM2_START, Sequence::Iterm2),
            ]
            .into_iter()
            .find(|(start, _)| rest.starts_with(start))
            .map(|(_, kind)| (at, kind))
        })
}

fn sequence_length(sequence: &[u8], kind: Sequence) -> Option<usize> {
    let terminator = find(sequence, STRING_TERMINATOR).map(|at| at + STRING_TERMINATOR.len());
    if kind != Sequence::Iterm2 {
        return terminator;
    }
    let searched = terminator.map_or(sequence, |end| &sequence[..end]);
    let bell = searched
        .iter()
        .position(|&byte| byte == BELL)
        .map(|at| at + 1);
    match (terminator, bell) {
        (Some(st), Some(bell)) => Some(st.min(bell)),
        (st, bell) => st.or(bell),
    }
}

struct KittyControl {
    action: u8,
    medium: u8,
    width: u64,
    height: u64,
    more_chunks: bool,
}

fn kitty_control(body: &[u8]) -> KittyControl {
    let inner = &body[KITTY_START.len()..];
    let end = inner
        .iter()
        .position(|&byte| byte == b';' || byte == 0x1b)
        .unwrap_or(inner.len());
    let mut control = KittyControl {
        action: b't',
        medium: b'd',
        width: 0,
        height: 0,
        more_chunks: false,
    };
    for pair in inner[..end].split(|&byte| byte == b',') {
        let [key, b'=', value @ ..] = pair else {
            continue;
        };
        match key {
            b'a' => control.action = value.first().copied().unwrap_or(b't'),
            b't' => control.medium = value.first().copied().unwrap_or(b'd'),
            b's' => control.width = number(value),
            b'v' => control.height = number(value),
            b'm' => control.more_chunks = value == b"1",
            _ => {}
        }
    }
    control
}

fn sixel(body: &[u8], traffic: &mut Traffic) {
    let inner = &body[DCS_START.len()..];
    let Some(introducer) = inner
        .iter()
        .position(|byte| !(byte.is_ascii_digit() || *byte == b';'))
    else {
        return;
    };
    if inner[introducer] != b'q' {
        return;
    }
    traffic.sixel_frames += 1;
    let data = &inner[introducer + 1..];
    let Some(raster) = data.strip_prefix(b"\"") else {
        return;
    };
    let end = raster
        .iter()
        .position(|byte| !(byte.is_ascii_digit() || *byte == b';'))
        .unwrap_or(raster.len());
    let fields: Vec<u64> = raster[..end]
        .split(|&byte| byte == b';')
        .map(number)
        .collect();
    if let [_, _, width, height] = fields[..] {
        traffic.sixel_pixels += width * height;
    }
}

fn number(digits: &[u8]) -> u64 {
    std::str::from_utf8(digits)
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(0)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn u64_from(length: usize) -> u64 {
    u64::try_from(length).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(stream: &[u8]) -> Traffic {
        between(&scan(stream), 0..stream.len())
    }

    #[test]
    fn plain_text_is_counted_as_bytes_without_graphics() {
        let stream = b"\x1b[2J\x1b[Hpage 1/16";
        let traffic = read(stream);
        assert_eq!(traffic.bytes, u64_from(stream.len()));
        assert_eq!(traffic.graphics_bytes, 0);
        assert_eq!(traffic.images_sent(), 0);
    }

    #[test]
    fn an_inline_kitty_transmission_counts_its_pixels() {
        let traffic = read(b"\x1b_Gq=2,a=T,U=1,i=7,p=1,f=32,t=d,s=640,v=480,o=z;AAAA\x1b\\");
        assert_eq!(traffic.transmissions, 1);
        assert_eq!(traffic.transmitted_pixels, 640 * 480);
        assert_eq!(traffic.out_of_band_transmissions, 0);
    }

    #[test]
    fn a_chunked_transmission_counts_once_and_all_its_bytes() {
        let stream =
            b"\x1b_Ga=t,f=32,s=10,v=10,m=1;AAAA\x1b\\\x1b_Gm=1;BBBB\x1b\\\x1b_Gm=0;CC\x1b\\";
        let traffic = read(stream);
        assert_eq!(traffic.transmissions, 1);
        assert_eq!(traffic.transmitted_pixels, 100);
        assert_eq!(traffic.graphics_bytes, traffic.bytes);
    }

    #[test]
    fn a_sequence_belongs_to_the_range_it_starts_in() {
        let stream = b"ab\x1b_Ga=t,f=32,s=10,v=10;AAAA\x1b\\cd";
        let marks = scan(stream);
        let first = between(&marks, 0..4);
        let second = between(&marks, 4..stream.len());
        assert_eq!(first.transmissions, 1);
        assert_eq!(second.transmissions, 0);
        assert_eq!(first.bytes + second.bytes, u64_from(stream.len()));
    }

    #[test]
    fn an_unfinished_sequence_at_the_end_is_not_counted() {
        let stream = b"\x1b_Ga=t,f=32,s=10,v=10;AAAA";
        let traffic = read(stream);
        assert_eq!(traffic.transmissions, 0);
        assert_eq!(traffic.graphics_bytes, 0);
        assert_eq!(traffic.bytes, u64_from(stream.len()));
    }

    #[test]
    fn shared_memory_and_file_transmissions_are_out_of_band() {
        let stream = b"\x1b_Ga=T,f=32,t=s,s=100,v=50;L3BpeGVs\x1b\\\x1b_Ga=t,f=24,t=f,s=20,v=20;L3RtcC9m\x1b\\";
        let traffic = read(stream);
        assert_eq!(traffic.transmissions, 2);
        assert_eq!(traffic.out_of_band_transmissions, 2);
        assert_eq!(traffic.transmitted_pixels, 100 * 50 + 20 * 20);
    }

    #[test]
    fn placements_and_deletions_send_no_pixels() {
        let stream = b"\x1b_Ga=p,i=3,p=1,x=0,y=40\x1b\\\x1b_Ga=d,d=i,i=3\x1b\\";
        let traffic = read(stream);
        assert_eq!(traffic.placements, 1);
        assert_eq!(traffic.deletions, 1);
        assert_eq!(traffic.pixels_sent(), 0);
    }

    #[test]
    fn a_sixel_frame_counts_its_raster_size() {
        let traffic = read(b"\x1b[1;1H\x1bP9;1;1q\"1;1;800;600#0;2;100;100;100#0~~$-\x1b\\");
        assert_eq!(traffic.sixel_frames, 1);
        assert_eq!(traffic.sixel_pixels, 800 * 600);
    }

    #[test]
    fn a_dcs_that_is_not_sixel_is_not_a_frame() {
        let traffic = read(b"\x1bP>|xterm(388)\x1b\\");
        assert_eq!(traffic.sixel_frames, 0);
    }

    #[test]
    fn a_long_stream_of_tiles_is_scanned_in_one_pass() {
        let tile =
            b"\x1b_Ga=T,f=32,t=d,s=64,v=48;QUFBQUFBQUFBQUFBQUFBQQ==\x1b\\\x1b[1;1H\xf4\x8e\xbb\xae";
        let stream: Vec<u8> = tile
            .iter()
            .copied()
            .cycle()
            .take(tile.len() * 200_000)
            .collect();
        let started = std::time::Instant::now();
        let traffic = read(&stream);
        assert_eq!(traffic.transmissions, 200_000);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn an_iterm2_image_ending_in_a_bell_is_one_image() {
        let traffic = read(b"\x1b]1337;File=inline=1;size=4:AAAA\x07after");
        assert_eq!(traffic.iterm2_images, 1);
        assert_eq!(traffic.graphics_bytes, traffic.bytes - 5);
    }
}
