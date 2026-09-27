use anyhow::Result;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder, RgbImage};

use crate::layout::Pane;

pub fn encode(frame: &RgbImage, pane: Pane) -> Result<String> {
    let mut png = Vec::new();
    PngEncoder::new_with_quality(&mut png, CompressionType::Fast, FilterType::Adaptive)
        .write_image(
            frame.as_raw(),
            frame.width(),
            frame.height(),
            ExtendedColorType::Rgb8,
        )?;
    let mut sequence = format!(
        "\x1b]1337;File=inline=1;size={};width={};height={};preserveAspectRatio=0:",
        png.len(),
        pane.columns,
        pane.rows
    );
    base64_simd::STANDARD.encode_append(&png, &mut sequence);
    sequence.push('\x07');
    Ok(sequence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    const PANE: Pane = Pane {
        columns: 2,
        rows: 1,
    };

    fn png_inside(sequence: &str) -> RgbImage {
        let (_, data) = sequence
            .strip_suffix('\x07')
            .unwrap()
            .split_once(':')
            .unwrap();
        let png = base64_simd::STANDARD.decode_to_vec(data).unwrap();
        image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .unwrap()
            .to_rgb8()
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
            encode(&frame, PANE).unwrap(),
            "\x1b]1337;File=inline=1;size=94;width=2;height=1;preserveAspectRatio=0:\
             iVBORw0KGgoAAAANSUhEUgAAAAQAAAACCAIAAADwyuo0AAAAJUlEQVR4AQEaAOX/AP///////wAAAAAA\
             AAD///////8AAAAAAAC/Wgv1zZ76ggAAAABJRU5ErkJggg==\x07"
        );
    }

    #[test]
    fn the_image_is_stretched_over_exactly_the_pane() {
        let sequence = encode(
            &RgbImage::new(800, 480),
            Pane {
                columns: 80,
                rows: 24,
            },
        )
        .unwrap();
        let (arguments, _) = sequence.split_once(':').unwrap();
        assert!(arguments.starts_with("\x1b]1337;File=inline=1;size="));
        assert!(
            arguments.ends_with(";width=80;height=24;preserveAspectRatio=0"),
            "{arguments}"
        );
    }

    #[test]
    fn the_png_round_trip_gives_back_every_pixel() {
        let frame = RgbImage::from_fn(97, 41, |x, y| {
            let level = u8::try_from((x * 7 + y * 13) % 256).unwrap();
            Rgb([level, 255 - level, level / 2])
        });
        assert_eq!(png_inside(&encode(&frame, PANE).unwrap()), frame);
    }

    #[test]
    fn the_size_argument_counts_the_png_bytes() {
        let frame = RgbImage::from_pixel(30, 20, Rgb([10, 20, 30]));
        let sequence = encode(&frame, PANE).unwrap();
        let (arguments, data) = sequence.split_once(':').unwrap();
        let size: usize = arguments
            .split(';')
            .find_map(|argument| argument.strip_prefix("size="))
            .unwrap()
            .parse()
            .unwrap();
        let png = base64_simd::STANDARD
            .decode_to_vec(data.strip_suffix('\x07').unwrap())
            .unwrap();
        assert_eq!(size, png.len());
    }
}
