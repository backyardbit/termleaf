use anyhow::Result;
use icy_sixel::{BackgroundMode, EncodeOptions, SixelImage};
use image::RgbImage;

const COLOURS: u16 = 256;

pub fn encode(frame: &RgbImage) -> Result<String> {
    let mut rgba = Vec::with_capacity(frame.as_raw().len() / 3 * 4);
    for pixel in frame.as_raw().as_chunks::<3>().0 {
        rgba.extend_from_slice(pixel);
        rgba.push(u8::MAX);
    }
    let image = SixelImage {
        background_mode: BackgroundMode::Transparent,
        ..SixelImage::try_from_rgba(
            rgba,
            usize::try_from(frame.width())?,
            usize::try_from(frame.height())?,
        )?
    };
    let options = EncodeOptions {
        max_colors: COLOURS,
        diffusion: 0.0,
        ..EncodeOptions::default()
    };
    Ok(image.encode_with(&options)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    fn decoded(sixel: &str, width: u32, height: u32) -> RgbImage {
        let image = SixelImage::decode(sixel.as_bytes()).unwrap();
        RgbImage::from_fn(width, height, |x, y| {
            let at = (usize::try_from(y).unwrap() * image.width + usize::try_from(x).unwrap()) * 4;
            Rgb([image.pixels[at], image.pixels[at + 1], image.pixels[at + 2]])
        })
    }

    fn round_trip(frame: &RgbImage) -> RgbImage {
        decoded(&encode(frame).unwrap(), frame.width(), frame.height())
    }

    fn checkerboard(width: u32, height: u32, colours: &[Rgb<u8>]) -> RgbImage {
        let count = u32::try_from(colours.len()).unwrap();
        RgbImage::from_fn(width, height, |x, y| {
            colours[usize::try_from((x / 3 + y / 2) % count).unwrap()]
        })
    }

    fn psnr(sent: &RgbImage, received: &RgbImage) -> f64 {
        let squared: f64 = sent
            .as_raw()
            .iter()
            .zip(received.as_raw())
            .map(|(a, b)| f64::from(a.abs_diff(*b)).powi(2))
            .sum();
        let mean = squared / f64::from(u32::try_from(sent.as_raw().len()).unwrap());
        10.0 * (255.0 * 255.0 / mean).log10()
    }

    #[test]
    fn a_tiny_frame_encodes_to_known_bytes() {
        let frame = RgbImage::from_fn(4, 2, |x, _| {
            if x < 2 {
                Rgb([255, 255, 255])
            } else {
                Rgb([0, 0, 0])
            }
        });
        assert_eq!(
            encode(&frame).unwrap(),
            "\x1bP9;1;0q\"1;1;4;2#0;2;100;100;100#1;2;0;0;0#0BB??$#1??BB$-\x1b\\"
        );
    }

    #[test]
    fn the_frame_declares_its_size_and_leaves_the_pixels_below_it_alone() {
        let sixel = encode(&RgbImage::new(30, 25)).unwrap();
        assert!(sixel.starts_with("\x1bP9;1;0q\"1;1;30;25"), "{sixel:?}");
    }

    #[test]
    fn black_white_primaries_and_even_greys_survive_a_round_trip() {
        let colours: Vec<Rgb<u8>> = [
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [255, 255, 0],
            [0, 255, 255],
            [255, 0, 255],
        ]
        .into_iter()
        .chain((0..6u8).map(|level| [level * 51; 3]))
        .map(Rgb)
        .collect();
        let frame = checkerboard(61, 23, &colours);
        assert_eq!(round_trip(&frame), frame);
    }

    #[test]
    fn a_frame_of_many_colours_comes_back_above_30_db() {
        let colours: Vec<Rgb<u8>> = (0..200u8)
            .map(|index| Rgb([index, 255 - index, index.wrapping_mul(37)]))
            .collect();
        let frame = checkerboard(90, 40, &colours);
        let quality = psnr(&frame, &round_trip(&frame));
        assert!(quality >= 30.0, "{quality:.1} dB");
    }

    #[test]
    fn pixels_of_one_colour_stay_one_colour_without_dithering() {
        let frame = RgbImage::from_fn(120, 36, |x, y| {
            let block = u8::try_from((x / 4) + (y / 6) * 30).unwrap();
            Rgb([block, block.wrapping_mul(7), 255 - block])
        });
        let back = round_trip(&frame);
        let mut seen = std::collections::HashMap::new();
        for (sent, received) in frame.pixels().zip(back.pixels()) {
            assert_eq!(seen.entry(*sent).or_insert(*received), received);
        }
    }

    #[test]
    fn an_empty_frame_is_an_error() {
        assert!(encode(&RgbImage::new(0, 0)).is_err());
    }
}
