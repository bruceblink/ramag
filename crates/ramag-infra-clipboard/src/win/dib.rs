//! Windows DIB/PNG 互转与不可信图片资源限制。

use std::io::Cursor;

const MAX_ENCODED_BYTES: usize = 64 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_IMAGE_PIXELS: u64 = 64 * 1024 * 1024;
const MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;

/// PNG IHDR 固定偏移取宽高。
pub(super) fn png_dims(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24
        || png.get(..8)? != b"\x89PNG\r\n\x1a\n"
        || png.get(8..12)? != 13u32.to_be_bytes()
        || png.get(12..16)? != b"IHDR"
    {
        return None;
    }
    let width = u32::from_be_bytes(png.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(png.get(20..24)?.try_into().ok()?);
    (width != 0 && height != 0).then_some((width, height))
}

pub(super) fn image_dimensions_allowed((width, height): (u32, u32)) -> bool {
    width <= MAX_IMAGE_DIMENSION
        && height <= MAX_IMAGE_DIMENSION
        && u64::from(width) * u64::from(height) <= MAX_IMAGE_PIXELS
}

fn decode_image(bytes: &[u8], format: image::ImageFormat) -> Option<image::DynamicImage> {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    reader.decode().ok()
}

/// BMP 文件头中的像素偏移：14 字节文件头 + DIB 头 + 可选掩码 + 可选调色板。
fn file_pixel_offset(
    header_size: u32,
    bit_count: u16,
    compression: u32,
    colors_used: u32,
    dib_len: usize,
) -> Option<u32> {
    let masks = if header_size == 40 && matches!(bit_count, 16 | 32) {
        match compression {
            3 => 12u32, // BI_BITFIELDS
            6 => 16u32, // BI_ALPHABITFIELDS
            _ => 0,
        }
    } else {
        0
    };
    let palette_entries = if bit_count <= 8 {
        let maximum = 1u32.checked_shl(u32::from(bit_count))?;
        if colors_used == 0 {
            maximum
        } else {
            colors_used.min(maximum)
        }
    } else {
        colors_used
    };
    let palette = palette_entries.checked_mul(4)?;
    let offset = 14u32
        .checked_add(header_size)?
        .checked_add(masks)?
        .checked_add(palette)?;
    let file_size = u32::try_from(dib_len).ok()?.checked_add(14)?;
    (offset <= file_size).then_some(offset)
}

/// CF_DIB（无 BMP 文件头）补文件头、受限解码，再统一编码成 PNG。
pub(super) fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < 40 {
        return None;
    }
    let header_size = u32::from_le_bytes(dib.get(0..4)?.try_into().ok()?);
    if header_size < 40 || usize::try_from(header_size).ok()? > dib.len() {
        return None;
    }
    if u16::from_le_bytes(dib.get(12..14)?.try_into().ok()?) != 1 {
        return None;
    }
    let width = i32::from_le_bytes(dib.get(4..8)?.try_into().ok()?);
    let height = i32::from_le_bytes(dib.get(8..12)?.try_into().ok()?);
    let dimensions = (
        u32::try_from(width).ok()?,
        u32::try_from(height.checked_abs()?).ok()?,
    );
    if !image_dimensions_allowed(dimensions) {
        return None;
    }
    let bit_count = u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?);
    let compression = u32::from_le_bytes(dib.get(16..20)?.try_into().ok()?);
    let colors_used = u32::from_le_bytes(dib.get(32..36)?.try_into().ok()?);
    let pixel_offset =
        file_pixel_offset(header_size, bit_count, compression, colors_used, dib.len())?;
    let file_size = u32::try_from(dib.len()).ok()?.checked_add(14)?;

    let mut bmp = Vec::with_capacity(dib.len() + 14);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&file_size.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&pixel_offset.to_le_bytes());
    bmp.extend_from_slice(dib);
    let image = decode_image(&bmp, image::ImageFormat::Bmp)?;
    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    (png.len() <= MAX_ENCODED_BYTES).then_some(png)
}

/// PNG 编码为 BMP 后去掉 14 字节文件头，供 CF_DIB 使用。
pub(super) fn png_to_dib(png: &[u8]) -> Option<Vec<u8>> {
    let (width, height) = png_dims(png)?;
    let dib_bytes = u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(4)?;
    if dib_bytes > MAX_ENCODED_BYTES as u64 {
        return None;
    }
    let image = decode_image(png, image::ImageFormat::Png)?;
    let mut bmp = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bmp), image::ImageFormat::Bmp)
        .ok()?;
    (bmp.len() > 14 && bmp.len() - 14 <= MAX_ENCODED_BYTES).then(|| bmp[14..].to_vec())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{dib_to_png, file_pixel_offset, image_dimensions_allowed, png_dims, png_to_dib};

    #[test]
    fn png_header_dimensions_are_checked() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&800u32.to_be_bytes());
        png.extend_from_slice(&600u32.to_be_bytes());
        assert_eq!(png_dims(&png), Some((800, 600)));
        png[0] = 0;
        assert_eq!(png_dims(&png), None);
    }

    #[test]
    fn image_dimensions_are_bounded() {
        assert!(image_dimensions_allowed((8_192, 4_320)));
        assert!(!image_dimensions_allowed((16_385, 1)));
        assert!(!image_dimensions_allowed((16_384, 16_384)));
    }

    #[test]
    fn pixel_offset_includes_masks_and_optional_palette() {
        assert_eq!(file_pixel_offset(40, 8, 0, 0, 2_000), Some(1_078));
        assert_eq!(file_pixel_offset(40, 24, 0, 2, 1_000), Some(62));
        assert_eq!(file_pixel_offset(40, 32, 3, 3, 1_000), Some(78));
        assert_eq!(file_pixel_offset(40, 32, 3, 300, 100), None);
    }

    #[test]
    fn png_and_dib_round_trip() {
        let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([12, 34, 56, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let dib = png_to_dib(&png).expect("encode DIB");
        let restored = dib_to_png(&dib).expect("decode DIB");
        assert_eq!(png_dims(&restored), Some((2, 2)));
    }
}
