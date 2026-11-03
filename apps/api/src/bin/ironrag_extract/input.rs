use std::io::{Cursor, Read};

use image::{DynamicImage, ImageFormat, ImageReader};
use ironrag_backend::integrations::extraction_protocol::{MAX_INPUT, MAX_PIXELS};

pub(super) fn pixels(width: u32, height: u32) -> anyhow::Result<()> {
    anyhow::ensure!(
        width > 0 && height > 0 && u64::from(width) * u64::from(height) <= MAX_PIXELS,
        "pixel_limit"
    );
    Ok(())
}

pub(super) fn read(path: &std::path::Path, limit: usize) -> anyhow::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    anyhow::ensure!(file.metadata()?.is_file(), "input must be a regular file");
    let mut data = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut data)?;
    anyhow::ensure!(data.len() <= limit, "file_limit");
    Ok(data)
}

pub(super) fn raster(bytes: &[u8]) -> anyhow::Result<DynamicImage> {
    anyhow::ensure!(bytes.len() <= MAX_INPUT, "image_bytes_limit");
    let dimensions =
        ImageReader::new(Cursor::new(bytes)).with_guessed_format()?.into_dimensions()?;
    pixels(dimensions.0, dimensions.1)?;
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_PIXELS * 8);
    reader.limits(limits);
    Ok(reader.decode()?)
}

pub(super) fn inspect_raster(bytes: &[u8]) -> anyhow::Result<()> {
    let (width, height) =
        ImageReader::new(Cursor::new(bytes)).with_guessed_format()?.into_dimensions()?;
    pixels(width, height)
}

pub(super) fn png(image: &DynamicImage) -> anyhow::Result<Vec<u8>> {
    let mut cursor = Cursor::new(Vec::new());
    image.write_to(&mut cursor, ImageFormat::Png)?;
    let bytes = cursor.into_inner();
    anyhow::ensure!(bytes.len() <= MAX_INPUT, "normalized_image_limit");
    Ok(bytes)
}

fn tiff(bytes: &[u8]) -> anyhow::Result<tiff::decoder::Decoder<Cursor<&[u8]>>> {
    let mut limits = tiff::decoder::Limits::default();
    limits.decoding_buffer_size = (MAX_PIXELS * 8) as usize;
    limits.intermediate_buffer_size = 32 * 1024 * 1024;
    Ok(tiff::decoder::Decoder::new(Cursor::new(bytes))?.with_limits(limits))
}

pub(super) fn tiff_count(bytes: &[u8], limit: u32) -> anyhow::Result<u32> {
    let mut decoder = tiff(bytes)?;
    let mut count = 1;
    loop {
        let (w, h) = decoder.dimensions()?;
        pixels(w, h)?;
        if !decoder.more_images() {
            return Ok(count);
        }
        anyhow::ensure!(count < limit, "frame_limit");
        decoder.next_image()?;
        count += 1;
    }
}

pub(super) fn tiff_frame(bytes: &[u8], ordinal: u32) -> anyhow::Result<Vec<u8>> {
    let mut decoder = tiff(bytes)?;
    for _ in 1..ordinal {
        decoder.next_image()?;
    }
    let (w, h) = decoder.dimensions()?;
    pixels(w, h)?;
    let color = decoder.colortype()?;
    let data = match decoder.read_image()? {
        tiff::decoder::DecodingResult::U8(data) => data,
        _ => anyhow::bail!("unsupported TIFF sample depth"),
    };
    let image = match color {
        tiff::ColorType::Gray(8) => DynamicImage::ImageLuma8(
            image::GrayImage::from_raw(w, h, data)
                .ok_or_else(|| anyhow::anyhow!("TIFF sample length"))?,
        ),
        tiff::ColorType::RGB(8) => DynamicImage::ImageRgb8(
            image::RgbImage::from_raw(w, h, data)
                .ok_or_else(|| anyhow::anyhow!("TIFF sample length"))?,
        ),
        tiff::ColorType::RGBA(8) => DynamicImage::ImageRgba8(
            image::RgbaImage::from_raw(w, h, data)
                .ok_or_else(|| anyhow::anyhow!("TIFF sample length"))?,
        ),
        _ => anyhow::bail!("unsupported TIFF color type"),
    };
    png(&image)
}

pub(super) fn pdf(bytes: &[u8]) -> anyhow::Result<lopdf::Document> {
    let document = lopdf::Document::load_mem(bytes)?;
    anyhow::ensure!(!document.is_encrypted(), "encrypted_pdf_not_supported");
    let pages = document.get_pages();
    anyhow::ensure!(!pages.is_empty(), "empty_pdf");
    anyhow::ensure!(pages.keys().copied().eq(1..=pages.len() as u32), "noncontiguous_pdf_pages");
    Ok(document)
}

pub(super) fn pdf_page(document: &lopdf::Document, ordinal: u32) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(document.get_pages().contains_key(&ordinal), "invalid_pdf_page");
    let mut page = document.clone();
    let discard: Vec<u32> = document.get_pages().into_keys().filter(|p| *p != ordinal).collect();
    page.delete_pages(&discard);
    page.prune_objects();
    anyhow::ensure!(page.get_pages().len() == 1, "pdf_split_count_mismatch");
    let mut bytes = Vec::new();
    page.save_to(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= MAX_INPUT, "split_pdf_limit");
    Ok(bytes)
}

/// Refuse unsafe page geometry before the renderer allocates its pixel buffer.
pub(super) fn pdf_render_pixels(
    document: &lopdf::Document,
    ordinal: u32,
    dpi: u32,
) -> anyhow::Result<()> {
    let mut id =
        *document.get_pages().get(&ordinal).ok_or_else(|| anyhow::anyhow!("invalid_pdf_page"))?;
    let mut media_box = None;
    let mut crop_box = None;
    let mut user_unit = None;
    let mut visited = std::collections::HashSet::new();
    let mut root_found = false;
    for _ in 0..64 {
        anyhow::ensure!(visited.insert(id), "invalid_pdf_page_tree");
        let dictionary = document.get_dictionary(id)?;
        if media_box.is_none() {
            media_box = dictionary.get(b"MediaBox").ok().cloned();
        }
        if crop_box.is_none() {
            crop_box = dictionary.get(b"CropBox").ok().cloned();
        }
        if user_unit.is_none() {
            user_unit = dictionary.get(b"UserUnit").ok().cloned();
        }
        match dictionary.get(b"Parent").ok() {
            Some(lopdf::Object::Reference(parent)) => id = *parent,
            _ => {
                root_found = true;
                break;
            }
        }
    }
    anyhow::ensure!(root_found, "pdf_page_tree_depth_limit");
    fn number(object: &lopdf::Object) -> anyhow::Result<f64> {
        let value = match object {
            lopdf::Object::Integer(value) => *value as f64,
            lopdf::Object::Real(value) => f64::from(*value),
            _ => anyhow::bail!("invalid_pdf_geometry"),
        };
        anyhow::ensure!(value.is_finite(), "invalid_pdf_geometry");
        Ok(value)
    }
    let unit = user_unit.as_ref().map(number).transpose()?.unwrap_or(1.0);
    anyhow::ensure!(unit > 0.0, "invalid_pdf_geometry");
    let media_box = media_box.ok_or_else(|| anyhow::anyhow!("pdf_media_box_missing"))?;
    for bounds in std::iter::once(media_box).chain(crop_box) {
        let bounds = match bounds {
            lopdf::Object::Reference(id) => document.get_object(id)?,
            ref object => object,
        };
        let coordinates = bounds.as_array()?;
        anyhow::ensure!(coordinates.len() == 4, "invalid_pdf_geometry");
        let width =
            ((number(&coordinates[2])? - number(&coordinates[0])?).abs() * unit * f64::from(dpi)
                / 72.0)
                .ceil();
        let height =
            ((number(&coordinates[3])? - number(&coordinates[1])?).abs() * unit * f64::from(dpi)
                / 72.0)
                .ceil();
        anyhow::ensure!(
            width.is_finite()
                && height.is_finite()
                && width >= 1.0
                && height >= 1.0
                && width * height <= MAX_PIXELS as f64,
            "pixel_limit"
        );
    }
    Ok(())
}

pub(super) fn unit_bytes(
    bytes: &[u8],
    pdf: Option<&lopdf::Document>,
    total: u32,
    mime: &str,
    ordinal: u32,
) -> anyhow::Result<Vec<u8>> {
    if let Some(document) = pdf {
        if total != 1 {
            return pdf_page(document, ordinal);
        }
        // Preserve recoverable streams when no PDF page needs to be removed.
    } else if mime == "image/tiff" {
        return tiff_frame(bytes, ordinal);
    }
    Ok(bytes.to_vec())
}
