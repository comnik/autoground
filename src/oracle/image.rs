//! One PNG or JPEG the oracle can attach to a state.
//!
//! The model accepts a file of at most 16 MiB, with each side from 280 to
//! 1568 and an area from 78_400 to 2_458_624. Before sending, the pixels are
//! rotated upright, a longest side over 1568 is scaled down, and an image
//! under the minimum is scaled up or padded on white. A progressive JPEG is
//! re-saved as baseline. The file on disk is unchanged. Bytes that already
//! fit, are already upright, and are not progressive are sent as they are.

use std::sync::Arc;

use sha2::{Digest, Sha256};

const MIN_SIDE: u32 = 280;
const MAX_SIDE: u32 = 1568;
const MIN_PIXELS: u64 = 78_400;
const MAX_PIXELS: u64 = 2_458_624;

pub(crate) const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageFormat {
    Png,
    Jpeg,
}

impl ImageFormat {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ImageError {
    Unsupported,
    TooSmall,
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Asset {
    reference: String,
    format: ImageFormat,
    bytes: Arc<[u8]>,
}

impl Asset {
    pub(crate) fn reference(&self) -> &str {
        &self.reference
    }

    pub(crate) fn format(&self) -> ImageFormat {
        self.format
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

pub(crate) fn image_extension(path: &str) -> Option<ImageFormat> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let ext = name.rsplit_once('.')?.1;
    if ext.eq_ignore_ascii_case("png") {
        Some(ImageFormat::Png)
    } else if ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg") {
        Some(ImageFormat::Jpeg)
    } else {
        None
    }
}

pub(crate) fn image_asset(bytes: &[u8], format: ImageFormat) -> Result<Asset, ImageError> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(ImageError::TooLarge);
    }
    let (width, height) = match format {
        ImageFormat::Png => png_size(bytes)?,
        ImageFormat::Jpeg => jpeg_size(bytes)?,
    };
    match prepare(bytes, format)? {
        Prepared::Original => {
            check_dimensions(width, height)?;
            Ok(asset_from(bytes, format))
        }
        Prepared::Encoded(out) => {
            let (width, height) = match format {
                ImageFormat::Png => png_size(&out)?,
                ImageFormat::Jpeg => jpeg_size(&out)?,
            };
            check_dimensions(width, height)?;
            Ok(asset_from(&out, format))
        }
    }
}

enum Prepared {
    Original,
    Encoded(Vec<u8>),
}

fn prepare(bytes: &[u8], format: ImageFormat) -> Result<Prepared, ImageError> {
    let Some((img, orientation)) = decode_oriented(bytes) else {
        return Ok(Prepared::Original);
    };
    let turned = orientation != image::metadata::Orientation::NoTransforms;
    let progressive = format == ImageFormat::Jpeg && jpeg_is_progressive(bytes);
    if !turned && !progressive && check_dimensions(img.width(), img.height()).is_ok() {
        return Ok(Prepared::Original);
    }
    let fitted = fit_image(img)?;
    let encoded = encode(&fitted, format)?;
    if encoded.len() > MAX_IMAGE_BYTES {
        return Err(ImageError::TooLarge);
    }
    Ok(Prepared::Encoded(encoded))
}

fn decode_oriented(bytes: &[u8]) -> Option<(image::DynamicImage, image::metadata::Orientation)> {
    use image::{DynamicImage, ImageDecoder, ImageReader};
    let reader = ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut decoder = reader.into_decoder().ok()?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).ok()?;
    image.apply_orientation(orientation);
    Some((image, orientation))
}

fn fit_image(mut img: image::DynamicImage) -> Result<image::DynamicImage, ImageError> {
    if img.width() == 0 || img.height() == 0 {
        return Err(ImageError::TooSmall);
    }
    if img.width().max(img.height()) > MAX_SIDE {
        img = img.resize(MAX_SIDE, MAX_SIDE, image::imageops::FilterType::Lanczos3);
    }
    if let Some((width, height)) = upscaled_size(img.width(), img.height()) {
        if (width, height) != (img.width(), img.height()) {
            img = img.resize_exact(width, height, image::imageops::FilterType::Lanczos3);
        }
        return Ok(img);
    }
    pad_to_minimum(img)
}

/// Uniform scale that meets the minimum without passing the maximum.
fn upscaled_size(width: u32, height: u32) -> Option<(u32, u32)> {
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return None;
    }
    if check_dimensions(width, height).is_ok() {
        return Some((width, height));
    }
    let w = u64::from(width);
    let h = u64::from(height);
    let (nw, nh) = if w <= h {
        (
            u64::from(MIN_SIDE),
            h.saturating_mul(u64::from(MIN_SIDE)).div_ceil(w),
        )
    } else {
        (
            w.saturating_mul(u64::from(MIN_SIDE)).div_ceil(h),
            u64::from(MIN_SIDE),
        )
    };
    if nw > u64::from(MAX_SIDE) || nh > u64::from(MAX_SIDE) || nw == 0 || nh == 0 {
        return None;
    }
    let (nw, nh) = (nw as u32, nh as u32);
    check_dimensions(nw, nh).ok()?;
    Some((nw, nh))
}

fn pad_to_minimum(img: image::DynamicImage) -> Result<image::DynamicImage, ImageError> {
    let (w, h) = (img.width(), img.height());
    let mut width = w.max(MIN_SIDE);
    let mut height = h.max(MIN_SIDE);
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(ImageError::TooLarge);
    }
    while u64::from(width) * u64::from(height) < MIN_PIXELS {
        if height < MAX_SIDE && (width >= MAX_SIDE || height <= width) {
            height += 1;
        } else if width < MAX_SIDE {
            width += 1;
        } else {
            return Err(ImageError::TooSmall);
        }
    }
    if (width, height) == (w, h) {
        return Ok(img);
    }
    Ok(pad_center(img, width, height))
}

fn pad_center(img: image::DynamicImage, width: u32, height: u32) -> image::DynamicImage {
    let x = i64::from((width - img.width()) / 2);
    let y = i64::from((height - img.height()) / 2);
    if img.has_alpha() {
        let mut canvas =
            image::RgbaImage::from_pixel(width, height, image::Rgba([255, 255, 255, 255]));
        image::imageops::overlay(&mut canvas, &img.to_rgba8(), x, y);
        image::DynamicImage::ImageRgba8(canvas)
    } else {
        let mut canvas = image::RgbImage::from_pixel(width, height, image::Rgb([255, 255, 255]));
        image::imageops::overlay(&mut canvas, &img.to_rgb8(), x, y);
        image::DynamicImage::ImageRgb8(canvas)
    }
}

fn encode(img: &image::DynamicImage, format: ImageFormat) -> Result<Vec<u8>, ImageError> {
    let mut out = Vec::new();
    match format {
        ImageFormat::Png => img
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .map_err(|_| ImageError::Unsupported)?,
        ImageFormat::Jpeg => {
            use image::ImageEncoder;
            let rgb = img.to_rgb8();
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90);
            encoder
                .write_image(
                    rgb.as_raw(),
                    rgb.width(),
                    rgb.height(),
                    image::ExtendedColorType::Rgb8,
                )
                .map_err(|_| ImageError::Unsupported)?;
        }
    }
    Ok(out)
}

fn asset_from(bytes: &[u8], format: ImageFormat) -> Asset {
    Asset {
        reference: reference_of(bytes),
        format,
        bytes: Arc::from(bytes),
    }
}

fn reference_of(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(4 + digest.len() * 2);
    out.push_str("img-");
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn check_dimensions(width: u32, height: u32) -> Result<(), ImageError> {
    let area = u64::from(width) * u64::from(height);
    if width > MAX_SIDE || height > MAX_SIDE || area > MAX_PIXELS {
        return Err(ImageError::TooLarge);
    }
    if width < MIN_SIDE || height < MIN_SIDE || area < MIN_PIXELS {
        return Err(ImageError::TooSmall);
    }
    Ok(())
}

fn png_size(bytes: &[u8]) -> Result<(u32, u32), ImageError> {
    if bytes.len() < PNG_SIGNATURE.len() || &bytes[..PNG_SIGNATURE.len()] != PNG_SIGNATURE {
        return Err(ImageError::Unsupported);
    }
    let mut i = PNG_SIGNATURE.len();
    let mut first = true;
    let mut size = None;
    while i + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
        let kind = &bytes[i + 4..i + 8];
        let data = i + 8;
        let next = data
            .checked_add(len)
            .and_then(|end| end.checked_add(4))
            .ok_or(ImageError::Unsupported)?;
        if next > bytes.len() {
            return Err(ImageError::Unsupported);
        }
        if first {
            if kind != b"IHDR" || len < 13 {
                return Err(ImageError::Unsupported);
            }
            let width = u32::from_be_bytes(bytes[data..data + 4].try_into().unwrap());
            let height = u32::from_be_bytes(bytes[data + 4..data + 8].try_into().unwrap());
            size = Some((width, height));
            first = false;
        }
        if kind == b"acTL" {
            return Err(ImageError::Unsupported);
        }
        if kind == b"IEND" {
            return size.ok_or(ImageError::Unsupported);
        }
        i = next;
    }
    Err(ImageError::Unsupported)
}

fn jpeg_size(bytes: &[u8]) -> Result<(u32, u32), ImageError> {
    let (width, height, _) = jpeg_frame(bytes)?;
    Ok((width, height))
}

fn jpeg_is_progressive(bytes: &[u8]) -> bool {
    jpeg_frame(bytes).is_ok_and(|(_, _, progressive)| progressive)
}

fn jpeg_frame(bytes: &[u8]) -> Result<(u32, u32, bool), ImageError> {
    if bytes.len() < 2 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return Err(ImageError::Unsupported);
    }
    let mut i = 2;
    while i < bytes.len() {
        if bytes[i] != 0xFF {
            return Err(ImageError::Unsupported);
        }
        while i < bytes.len() && bytes[i] == 0xFF {
            i += 1;
        }
        if i >= bytes.len() {
            return Err(ImageError::Unsupported);
        }
        let marker = bytes[i];
        i += 1;
        if marker == 0xD9 {
            return Err(ImageError::Unsupported);
        }
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        if i + 2 > bytes.len() {
            return Err(ImageError::Unsupported);
        }
        let len = u16::from_be_bytes([bytes[i], bytes[i + 1]]) as usize;
        if len < 2 || i + len > bytes.len() {
            return Err(ImageError::Unsupported);
        }
        if is_sof(marker) {
            if len < 7 {
                return Err(ImageError::Unsupported);
            }
            let height = u16::from_be_bytes([bytes[i + 3], bytes[i + 4]]);
            let width = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]);
            let progressive = matches!(marker, 0xC2 | 0xC6 | 0xCA | 0xCE);
            return Ok((u32::from(width), u32::from(height), progressive));
        }
        i += len;
    }
    Err(ImageError::Unsupported)
}

fn is_sof(marker: u8) -> bool {
    matches!(
        marker,
        0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF
    )
}

#[cfg(test)]
pub(crate) fn synthetic_png(width: u32, height: u32) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(PNG_SIGNATURE);
    data.extend_from_slice(&13u32.to_be_bytes());
    data.extend_from_slice(b"IHDR");
    data.extend_from_slice(&width.to_be_bytes());
    data.extend_from_slice(&height.to_be_bytes());
    data.extend_from_slice(&[8, 2, 0, 0, 0]);
    data.extend_from_slice(&[0, 0, 0, 0]);
    data.extend_from_slice(&0u32.to_be_bytes());
    data.extend_from_slice(b"IEND");
    data.extend_from_slice(&[0, 0, 0, 0]);
    data
}

#[cfg(test)]
pub(crate) fn solid_png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("png");
    out
}

#[cfg(test)]
pub(crate) fn solid_jpeg(width: u32, height: u32) -> Vec<u8> {
    use image::ImageEncoder;
    let image = image::RgbImage::from_pixel(width, height, image::Rgb([10, 20, 30]));
    let mut out = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90);
    encoder
        .write_image(
            image.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgb8,
        )
        .expect("jpeg");
    out
}

#[cfg(test)]
pub(crate) fn synthetic_jpeg(width: u16, height: u16) -> Vec<u8> {
    let mut data = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x0B, 0x08];
    data.extend_from_slice(&height.to_be_bytes());
    data.extend_from_slice(&width.to_be_bytes());
    data.extend_from_slice(&[1, 1, 0x11, 0, 0xFF, 0xD9]);
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg_segment(marker: u8, width: u16, height: u16) -> Vec<u8> {
        let mut data = vec![0xFF, 0xD8, 0xFF, marker, 0x00, 0x0B, 0x08];
        data.extend_from_slice(&height.to_be_bytes());
        data.extend_from_slice(&width.to_be_bytes());
        data.extend_from_slice(&[1, 1, 0x11, 0, 0xFF, 0xD9]);
        data
    }

    fn with_actl(mut png: Vec<u8>) -> Vec<u8> {
        let iend = png.split_off(png.len() - 12);
        png.extend_from_slice(&8u32.to_be_bytes());
        png.extend_from_slice(b"acTL");
        png.extend_from_slice(&2u32.to_be_bytes());
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(&[0, 0, 0, 0]);
        png.extend(iend);
        png
    }

    #[test]
    fn extension_picks_png_and_jpeg() {
        assert_eq!(image_extension("shot.PNG"), Some(ImageFormat::Png));
        assert_eq!(image_extension("dir/shot.jpg"), Some(ImageFormat::Jpeg));
        assert_eq!(image_extension("shot.JPEG"), Some(ImageFormat::Jpeg));
        assert_eq!(image_extension("shot.png.txt"), None);
        assert_eq!(image_extension("shot"), None);
    }

    #[test]
    fn png_and_jpeg_at_the_dimension_bounds_are_assets() {
        for (width, height) in [(280, 280), (1568, 1568)] {
            let png = image_asset(&synthetic_png(width, height), ImageFormat::Png).unwrap();
            assert_eq!(png.format().as_str(), "png");
            assert!(png.reference().starts_with("img-"));
            assert_eq!(png.reference().len(), 4 + 64);
            let jpeg = image_asset(
                &synthetic_jpeg(width as u16, height as u16),
                ImageFormat::Jpeg,
            )
            .unwrap();
            assert_eq!(jpeg.format().as_str(), "jpg");
        }
    }

    #[test]
    fn the_same_bytes_share_a_reference() {
        let png = synthetic_png(280, 280);
        let first = image_asset(&png, ImageFormat::Png).unwrap();
        let second = image_asset(&png, ImageFormat::Png).unwrap();
        assert_eq!(first.reference(), second.reference());
        assert_eq!(first.bytes(), png.as_slice());
    }

    #[test]
    fn dimensions_outside_the_range_are_rejected() {
        assert_eq!(
            image_asset(&synthetic_png(279, 280), ImageFormat::Png),
            Err(ImageError::TooSmall)
        );
        assert_eq!(
            image_asset(&synthetic_png(100, 100), ImageFormat::Png),
            Err(ImageError::TooSmall)
        );
        assert_eq!(
            image_asset(&synthetic_png(1569, 280), ImageFormat::Png),
            Err(ImageError::TooLarge)
        );
        assert_eq!(
            image_asset(&synthetic_png(2000, 100), ImageFormat::Png),
            Err(ImageError::TooLarge)
        );
        assert_eq!(
            image_asset(&synthetic_jpeg(100, 100), ImageFormat::Jpeg),
            Err(ImageError::TooSmall)
        );
    }

    #[test]
    fn a_mismatched_or_broken_image_is_unsupported() {
        assert_eq!(
            image_asset(&synthetic_jpeg(280, 280), ImageFormat::Png),
            Err(ImageError::Unsupported)
        );
        assert_eq!(
            image_asset(&synthetic_png(280, 280), ImageFormat::Jpeg),
            Err(ImageError::Unsupported)
        );
        assert_eq!(
            image_asset(b"\x89PNG\r\n\x1a\n", ImageFormat::Png),
            Err(ImageError::Unsupported)
        );
        assert_eq!(
            image_asset(&[0xFF, 0xD8], ImageFormat::Jpeg),
            Err(ImageError::Unsupported)
        );
        let mut png = synthetic_png(280, 280);
        png[12] = b'X';
        assert_eq!(
            image_asset(&png, ImageFormat::Png),
            Err(ImageError::Unsupported)
        );
        assert_eq!(
            image_asset(&with_actl(synthetic_png(280, 280)), ImageFormat::Png),
            Err(ImageError::Unsupported)
        );
    }

    #[test]
    fn jpeg_reads_the_start_of_frame_after_another_segment() {
        let mut data = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        data.extend_from_slice(b"JFIF\0");
        data.extend_from_slice(&[1, 1, 0, 0, 1, 0, 1, 0, 0]);
        let sof = jpeg_segment(0xC0, 300, 400);
        data.extend_from_slice(&sof[2..]);
        let asset = image_asset(&data, ImageFormat::Jpeg).unwrap();
        assert_eq!(asset.format(), ImageFormat::Jpeg);
        let progressive = jpeg_segment(0xC2, 280, 280);
        let asset = image_asset(&progressive, ImageFormat::Jpeg).unwrap();
        assert_eq!(asset.format(), ImageFormat::Jpeg);
        assert_eq!(asset.bytes(), progressive.as_slice());
        assert_eq!(
            image_asset(
                &[0xFF, 0xD8, 0xFF, 0xC4, 0x00, 0x02, 0xFF, 0xD9],
                ImageFormat::Jpeg
            ),
            Err(ImageError::Unsupported)
        );
    }

    #[test]
    fn an_oversized_image_is_scaled_down_to_fit() {
        let png = solid_png(2000, 1000);
        let asset = image_asset(&png, ImageFormat::Png).unwrap();
        assert_ne!(asset.bytes(), png.as_slice());
        assert_eq!(png_size(asset.bytes()).unwrap(), (1568, 784));
        let jpeg = solid_jpeg(2000, 1000);
        let asset = image_asset(&jpeg, ImageFormat::Jpeg).unwrap();
        assert_eq!(asset.format().as_str(), "jpg");
        assert_eq!(jpeg_size(asset.bytes()).unwrap(), (1568, 784));
    }

    #[test]
    fn an_image_that_already_fits_is_sent_unchanged() {
        let png = solid_png(400, 400);
        assert_eq!(
            image_asset(&png, ImageFormat::Png).unwrap().bytes(),
            png.as_slice()
        );
        let jpeg = solid_jpeg(400, 400);
        assert_eq!(
            image_asset(&jpeg, ImageFormat::Jpeg).unwrap().bytes(),
            jpeg.as_slice()
        );
    }

    #[test]
    fn an_image_under_the_minimum_is_scaled_up() {
        let png = solid_png(100, 100);
        let asset = image_asset(&png, ImageFormat::Png).unwrap();
        assert_eq!(png_size(asset.bytes()).unwrap(), (280, 280));
        let decoded = image::load_from_memory(asset.bytes()).unwrap().to_rgba8();
        assert!(decoded.get_pixel(0, 0).0[0] < 80);
        let jpeg = solid_jpeg(100, 100);
        let asset = image_asset(&jpeg, ImageFormat::Jpeg).unwrap();
        assert_eq!(jpeg_size(asset.bytes()).unwrap(), (280, 280));
    }

    #[test]
    fn a_wide_image_is_padded_on_white() {
        let png = solid_png(4000, 100);
        let asset = image_asset(&png, ImageFormat::Png).unwrap();
        assert_eq!(png_size(asset.bytes()).unwrap(), (1568, 280));
        let decoded = image::load_from_memory(asset.bytes()).unwrap().to_rgba8();
        assert_eq!(decoded.get_pixel(0, 0).0, [255, 255, 255, 255]);
        assert_eq!(decoded.get_pixel(0, 279).0, [255, 255, 255, 255]);
        let center = decoded.get_pixel(784, 140).0;
        assert!(center[0] < 80 && center[1] < 80 && center[2] < 80);
    }

    #[test]
    fn exif_orientation_rotates_the_pixels() {
        let stored = solid_jpeg(1568, 400);
        let jpeg = with_exif_orientation(&stored, 6);
        let asset = image_asset(&jpeg, ImageFormat::Jpeg).unwrap();
        assert_eq!(jpeg_size(asset.bytes()).unwrap(), (400, 1568));
        assert!(!asset.bytes().windows(6).any(|window| window == b"Exif\0\0"));
    }

    #[test]
    fn a_progressive_marker_is_recognized() {
        for marker in [0xC2, 0xC6, 0xCA, 0xCE] {
            assert!(jpeg_is_progressive(&jpeg_segment(marker, 280, 280)));
        }
        assert!(!jpeg_is_progressive(&solid_jpeg(280, 280)));
        assert!(!jpeg_is_progressive(&jpeg_segment(0xC0, 280, 280)));
    }

    fn with_exif_orientation(jpeg: &[u8], orientation: u16) -> Vec<u8> {
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II*\0");
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&0x0112u16.to_le_bytes());
        tiff.extend_from_slice(&3u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&orientation.to_le_bytes());
        tiff.extend_from_slice(&0u16.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        let len = u16::try_from(2 + 6 + tiff.len()).unwrap();
        let mut out = Vec::with_capacity(jpeg.len() + 4 + 6 + tiff.len());
        out.extend_from_slice(&jpeg[..2]);
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(b"Exif\0\0");
        out.extend_from_slice(&tiff);
        out.extend_from_slice(&jpeg[2..]);
        out
    }
}
