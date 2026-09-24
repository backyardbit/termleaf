use std::fmt::Write as _;
use std::io::Write as _;

use image::RgbaImage;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::Widget;

const PLACEHOLDER: char = '\u{10EEEE}';
const BASE64_CHUNK: usize = 4096;
const RAW_CHUNK: usize = BASE64_CHUNK / 4 * 3;
const MAX_ID: u32 = 0x00FF_FFFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageId(u32);

impl ImageId {
    pub fn first() -> Self {
        Self(1)
    }

    pub fn next(self) -> Self {
        if self.0 >= MAX_ID {
            Self::first()
        } else {
            Self(self.0 + 1)
        }
    }

    fn colour(self) -> Color {
        let [_, red, green, blue] = self.0.to_be_bytes();
        Color::Rgb(red, green, blue)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Payload {
    Raw,
    Zlib,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellGrid {
    pub columns: u16,
    pub rows: u16,
}

pub fn transmit(id: ImageId, image: &RgbaImage, grid: CellGrid, payload: Payload) -> String {
    let (bytes, encoding) = match payload {
        Payload::Raw => (image.as_raw().clone(), ""),
        Payload::Zlib => (zlib(image.as_raw()), "o=z,"),
    };
    let chunks: Vec<&[u8]> = bytes.chunks(RAW_CHUNK).collect();
    let mut sequence = String::new();
    for (index, chunk) in chunks.iter().enumerate() {
        sequence.push_str("\x1b_Gq=2,");
        if index == 0 {
            let _ = write!(
                sequence,
                "a=T,U=1,i={},f=32,t=d,s={},v={},c={},r={},{encoding}",
                id.0,
                image.width(),
                image.height(),
                grid.columns,
                grid.rows
            );
        }
        let more = u8::from(index + 1 < chunks.len());
        let _ = write!(sequence, "m={more};");
        base64_simd::STANDARD.encode_append(chunk, &mut sequence);
        sequence.push_str("\x1b\\");
    }
    sequence
}

fn zlib(raw: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(
        Vec::with_capacity(raw.len() / 64),
        flate2::Compression::fast(),
    );
    if encoder.write_all(raw).is_err() {
        return Vec::new();
    }
    encoder.finish().unwrap_or_default()
}

pub fn delete(id: ImageId) -> String {
    format!("\x1b_Gq=2,a=d,d=I,i={}\x1b\\", id.0)
}

pub struct Placeholders {
    pub id: ImageId,
    pub first_column: u16,
    pub first_row: u16,
}

impl Widget for Placeholders {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let mut symbol = String::new();
        for y in 0..area.height {
            let Some(row) = diacritic(self.first_row.saturating_add(y)) else {
                continue;
            };
            for x in 0..area.width {
                let Some(column) = diacritic(self.first_column.saturating_add(x)) else {
                    continue;
                };
                let Some(cell) = buffer.cell_mut((area.x + x, area.y + y)) else {
                    continue;
                };
                symbol.clear();
                symbol.push(PLACEHOLDER);
                symbol.push(row);
                symbol.push(column);
                cell.set_symbol(&symbol)
                    .set_fg(self.id.colour())
                    .set_diff_option(CellDiffOption::ForcedWidth(std::num::NonZeroU16::MIN));
            }
        }
    }
}

fn diacritic(position: u16) -> Option<char> {
    DIACRITICS.get(usize::from(position)).copied()
}

static DIACRITICS: [char; 297] = [
    '\u{305}',
    '\u{30D}',
    '\u{30E}',
    '\u{310}',
    '\u{312}',
    '\u{33D}',
    '\u{33E}',
    '\u{33F}',
    '\u{346}',
    '\u{34A}',
    '\u{34B}',
    '\u{34C}',
    '\u{350}',
    '\u{351}',
    '\u{352}',
    '\u{357}',
    '\u{35B}',
    '\u{363}',
    '\u{364}',
    '\u{365}',
    '\u{366}',
    '\u{367}',
    '\u{368}',
    '\u{369}',
    '\u{36A}',
    '\u{36B}',
    '\u{36C}',
    '\u{36D}',
    '\u{36E}',
    '\u{36F}',
    '\u{483}',
    '\u{484}',
    '\u{485}',
    '\u{486}',
    '\u{487}',
    '\u{592}',
    '\u{593}',
    '\u{594}',
    '\u{595}',
    '\u{597}',
    '\u{598}',
    '\u{599}',
    '\u{59C}',
    '\u{59D}',
    '\u{59E}',
    '\u{59F}',
    '\u{5A0}',
    '\u{5A1}',
    '\u{5A8}',
    '\u{5A9}',
    '\u{5AB}',
    '\u{5AC}',
    '\u{5AF}',
    '\u{5C4}',
    '\u{610}',
    '\u{611}',
    '\u{612}',
    '\u{613}',
    '\u{614}',
    '\u{615}',
    '\u{616}',
    '\u{617}',
    '\u{657}',
    '\u{658}',
    '\u{659}',
    '\u{65A}',
    '\u{65B}',
    '\u{65D}',
    '\u{65E}',
    '\u{6D6}',
    '\u{6D7}',
    '\u{6D8}',
    '\u{6D9}',
    '\u{6DA}',
    '\u{6DB}',
    '\u{6DC}',
    '\u{6DF}',
    '\u{6E0}',
    '\u{6E1}',
    '\u{6E2}',
    '\u{6E4}',
    '\u{6E7}',
    '\u{6E8}',
    '\u{6EB}',
    '\u{6EC}',
    '\u{730}',
    '\u{732}',
    '\u{733}',
    '\u{735}',
    '\u{736}',
    '\u{73A}',
    '\u{73D}',
    '\u{73F}',
    '\u{740}',
    '\u{741}',
    '\u{743}',
    '\u{745}',
    '\u{747}',
    '\u{749}',
    '\u{74A}',
    '\u{7EB}',
    '\u{7EC}',
    '\u{7ED}',
    '\u{7EE}',
    '\u{7EF}',
    '\u{7F0}',
    '\u{7F1}',
    '\u{7F3}',
    '\u{816}',
    '\u{817}',
    '\u{818}',
    '\u{819}',
    '\u{81B}',
    '\u{81C}',
    '\u{81D}',
    '\u{81E}',
    '\u{81F}',
    '\u{820}',
    '\u{821}',
    '\u{822}',
    '\u{823}',
    '\u{825}',
    '\u{826}',
    '\u{827}',
    '\u{829}',
    '\u{82A}',
    '\u{82B}',
    '\u{82C}',
    '\u{82D}',
    '\u{951}',
    '\u{953}',
    '\u{954}',
    '\u{F82}',
    '\u{F83}',
    '\u{F86}',
    '\u{F87}',
    '\u{135D}',
    '\u{135E}',
    '\u{135F}',
    '\u{17DD}',
    '\u{193A}',
    '\u{1A17}',
    '\u{1A75}',
    '\u{1A76}',
    '\u{1A77}',
    '\u{1A78}',
    '\u{1A79}',
    '\u{1A7A}',
    '\u{1A7B}',
    '\u{1A7C}',
    '\u{1B6B}',
    '\u{1B6D}',
    '\u{1B6E}',
    '\u{1B6F}',
    '\u{1B70}',
    '\u{1B71}',
    '\u{1B72}',
    '\u{1B73}',
    '\u{1CD0}',
    '\u{1CD1}',
    '\u{1CD2}',
    '\u{1CDA}',
    '\u{1CDB}',
    '\u{1CE0}',
    '\u{1DC0}',
    '\u{1DC1}',
    '\u{1DC3}',
    '\u{1DC4}',
    '\u{1DC5}',
    '\u{1DC6}',
    '\u{1DC7}',
    '\u{1DC8}',
    '\u{1DC9}',
    '\u{1DCB}',
    '\u{1DCC}',
    '\u{1DD1}',
    '\u{1DD2}',
    '\u{1DD3}',
    '\u{1DD4}',
    '\u{1DD5}',
    '\u{1DD6}',
    '\u{1DD7}',
    '\u{1DD8}',
    '\u{1DD9}',
    '\u{1DDA}',
    '\u{1DDB}',
    '\u{1DDC}',
    '\u{1DDD}',
    '\u{1DDE}',
    '\u{1DDF}',
    '\u{1DE0}',
    '\u{1DE1}',
    '\u{1DE2}',
    '\u{1DE3}',
    '\u{1DE4}',
    '\u{1DE5}',
    '\u{1DE6}',
    '\u{1DFE}',
    '\u{20D0}',
    '\u{20D1}',
    '\u{20D4}',
    '\u{20D5}',
    '\u{20D6}',
    '\u{20D7}',
    '\u{20DB}',
    '\u{20DC}',
    '\u{20E1}',
    '\u{20E7}',
    '\u{20E9}',
    '\u{20F0}',
    '\u{2CEF}',
    '\u{2CF0}',
    '\u{2CF1}',
    '\u{2DE0}',
    '\u{2DE1}',
    '\u{2DE2}',
    '\u{2DE3}',
    '\u{2DE4}',
    '\u{2DE5}',
    '\u{2DE6}',
    '\u{2DE7}',
    '\u{2DE8}',
    '\u{2DE9}',
    '\u{2DEA}',
    '\u{2DEB}',
    '\u{2DEC}',
    '\u{2DED}',
    '\u{2DEE}',
    '\u{2DEF}',
    '\u{2DF0}',
    '\u{2DF1}',
    '\u{2DF2}',
    '\u{2DF3}',
    '\u{2DF4}',
    '\u{2DF5}',
    '\u{2DF6}',
    '\u{2DF7}',
    '\u{2DF8}',
    '\u{2DF9}',
    '\u{2DFA}',
    '\u{2DFB}',
    '\u{2DFC}',
    '\u{2DFD}',
    '\u{2DFE}',
    '\u{2DFF}',
    '\u{A66F}',
    '\u{A67C}',
    '\u{A67D}',
    '\u{A6F0}',
    '\u{A6F1}',
    '\u{A8E0}',
    '\u{A8E1}',
    '\u{A8E2}',
    '\u{A8E3}',
    '\u{A8E4}',
    '\u{A8E5}',
    '\u{A8E6}',
    '\u{A8E7}',
    '\u{A8E8}',
    '\u{A8E9}',
    '\u{A8EA}',
    '\u{A8EB}',
    '\u{A8EC}',
    '\u{A8ED}',
    '\u{A8EE}',
    '\u{A8EF}',
    '\u{A8F0}',
    '\u{A8F1}',
    '\u{AAB0}',
    '\u{AAB2}',
    '\u{AAB3}',
    '\u{AAB7}',
    '\u{AAB8}',
    '\u{AABE}',
    '\u{AABF}',
    '\u{AAC1}',
    '\u{FE20}',
    '\u{FE21}',
    '\u{FE22}',
    '\u{FE23}',
    '\u{FE24}',
    '\u{FE25}',
    '\u{FE26}',
    '\u{10A0F}',
    '\u{10A38}',
    '\u{1D185}',
    '\u{1D186}',
    '\u{1D187}',
    '\u{1D188}',
    '\u{1D189}',
    '\u{1D1AA}',
    '\u{1D1AB}',
    '\u{1D1AC}',
    '\u{1D1AD}',
    '\u{1D242}',
    '\u{1D243}',
    '\u{1D244}',
];

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(sequence: &str) -> Vec<u8> {
        sequence
            .split("\x1b\\")
            .filter_map(|chunk| chunk.split_once(';').map(|(_, data)| data))
            .flat_map(|data| base64_simd::STANDARD.decode_to_vec(data).unwrap())
            .collect()
    }

    #[test]
    fn a_transmission_carries_the_raw_pixels() {
        let image = RgbaImage::from_pixel(70, 50, image::Rgba([1, 2, 3, 4]));
        let sequence = transmit(
            ImageId::first(),
            &image,
            CellGrid {
                columns: 7,
                rows: 5,
            },
            Payload::Raw,
        );
        assert_eq!(payload(&sequence), image.as_raw().clone());
    }

    #[test]
    fn a_transmission_creates_a_virtual_placement_of_the_given_cells() {
        let image = RgbaImage::new(70, 50);
        let sequence = transmit(
            ImageId::first(),
            &image,
            CellGrid {
                columns: 7,
                rows: 5,
            },
            Payload::Raw,
        );
        assert!(sequence.starts_with("\x1b_Gq=2,a=T,U=1,i=1,f=32,t=d,s=70,v=50,c=7,r=5,"));
    }

    #[test]
    fn only_the_last_chunk_ends_the_transmission() {
        let image = RgbaImage::new(100, 100);
        let sequence = transmit(
            ImageId::first(),
            &image,
            CellGrid {
                columns: 10,
                rows: 10,
            },
            Payload::Raw,
        );
        let endings: Vec<&str> = sequence
            .match_indices("m=")
            .map(|(at, _)| &sequence[at..at + 3])
            .collect();
        assert!(endings.len() > 1);
        assert_eq!(endings.last(), Some(&"m=0"));
        assert!(
            endings[..endings.len() - 1]
                .iter()
                .all(|ending| *ending == "m=1")
        );
    }

    fn inflate(bytes: &[u8]) -> Vec<u8> {
        use std::io::Read;
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(bytes)
            .read_to_end(&mut raw)
            .unwrap();
        raw
    }

    #[test]
    fn a_compressed_transmission_inflates_back_to_the_raw_pixels() {
        let image = RgbaImage::from_fn(70, 50, |x, y| {
            image::Rgba([u8::try_from(x).unwrap(), u8::try_from(y).unwrap(), 3, 255])
        });
        let sequence = transmit(
            ImageId::first(),
            &image,
            CellGrid {
                columns: 7,
                rows: 5,
            },
            Payload::Zlib,
        );
        assert_eq!(inflate(&payload(&sequence)), image.as_raw().clone());
    }

    #[test]
    fn a_compressed_transmission_says_so_and_keeps_the_raw_size() {
        let image = RgbaImage::new(70, 50);
        let sequence = transmit(
            ImageId::first(),
            &image,
            CellGrid {
                columns: 7,
                rows: 5,
            },
            Payload::Zlib,
        );
        assert!(sequence.starts_with("\x1b_Gq=2,a=T,U=1,i=1,f=32,t=d,s=70,v=50,c=7,r=5,o=z,"));
    }

    #[test]
    fn a_blank_page_compresses_to_a_tiny_fraction() {
        let image = RgbaImage::from_pixel(1280, 1920, image::Rgba([255, 255, 255, 255]));
        let grid = CellGrid {
            columns: 64,
            rows: 48,
        };
        let raw = transmit(ImageId::first(), &image, grid, Payload::Raw);
        let compressed = transmit(ImageId::first(), &image, grid, Payload::Zlib);
        assert!(compressed.len() * 100 < raw.len());
    }

    #[test]
    fn delete_frees_the_image_data() {
        assert_eq!(delete(ImageId::first()), "\x1b_Gq=2,a=d,d=I,i=1\x1b\\");
    }

    #[test]
    fn ids_wrap_before_they_leave_the_colour_range() {
        assert_eq!(ImageId(MAX_ID).next(), ImageId::first());
        assert_eq!(ImageId::first().next(), ImageId(2));
    }

    fn marks(buffer: &Buffer, x: u16, y: u16) -> Vec<char> {
        buffer[(x, y)].symbol().chars().collect()
    }

    #[test]
    fn placeholders_start_at_the_given_row_and_column_of_the_image() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 3));
        Placeholders {
            id: ImageId(0x0001_0203),
            first_column: 5,
            first_row: 2,
        }
        .render(Rect::new(1, 1, 2, 2), &mut buffer);
        assert_eq!(
            marks(&buffer, 1, 1),
            [PLACEHOLDER, DIACRITICS[2], DIACRITICS[5]]
        );
        assert_eq!(
            marks(&buffer, 2, 2),
            [PLACEHOLDER, DIACRITICS[3], DIACRITICS[6]]
        );
        assert_eq!(buffer[(1, 1)].fg, Color::Rgb(1, 2, 3));
        assert_eq!(buffer[(0, 0)].symbol(), " ");
    }

    #[test]
    fn positions_past_the_diacritic_table_stay_blank() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 2, 1));
        Placeholders {
            id: ImageId::first(),
            first_column: 296,
            first_row: 0,
        }
        .render(Rect::new(0, 0, 2, 1), &mut buffer);
        assert_eq!(marks(&buffer, 0, 0)[0], PLACEHOLDER);
        assert_eq!(buffer[(1, 0)].symbol(), " ");
    }
}
