use std::io::Cursor;

use image::{DynamicImage, ImageFormat};
use time::Instant;
use tracing::{debug, error, info, instrument};

use crate::{hash::Hash, ImageKind, PKAvatarError};

const MAX_DIMENSION: u32 = 4000;

pub struct ProcessOutput {
    pub width: u32,
    pub height: u32,
    pub hash: Hash,
    pub data_webp: Vec<u8>,
}

// Moving Vec<u8> in here since the thread needs ownership of it now, it's fine, don't need it after
pub async fn process_async(data: Vec<u8>, kind: ImageKind) -> Result<ProcessOutput, PKAvatarError> {
    tokio::task::spawn_blocking(move || process(&data, kind)).await
        .map_err(|je| PKAvatarError::InternalError(je.into()))?
}
#[instrument(skip_all)]
pub fn process(data: &[u8], kind: ImageKind) -> Result<ProcessOutput, PKAvatarError> {
    let time_before = Instant::now();
    let reader = reader_for(data);
    match reader.format() {
        Some(ImageFormat::Png | ImageFormat::Gif | ImageFormat::WebP | ImageFormat::Jpeg | ImageFormat::Tiff) => {} // ok :)
        Some(other) => return Err(PKAvatarError::UnsupportedImageFormat(other)),
        None => return Err(PKAvatarError::UnknownImageFormat),
    }

    // want to check dimensions *before* decoding so we don't accidentally end up with a memory bomb
    // eg. a 16000x16000 png file is only 31kb and expands to almost a gig of memory
    let (width, height) = reader.into_dimensions()?;
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(PKAvatarError::ImageDimensionsTooLarge(
            (width, height),
            (MAX_DIMENSION, MAX_DIMENSION),
        ));
    }

    // need to make a new reader??? why can't it just use the same one. reduce duplication?
    let reader = reader_for(data);

    let time_after_parse = Instant::now();

    let image = reader.decode().map_err(|e| {
        // print the ugly error, return the nice error
        error!("error decoding image: {}", e);
        PKAvatarError::ImageFormatError(e)
    })?;
    let time_after_decode = Instant::now();
    let image = resize(image, kind);
    let time_after_resize = Instant::now();

    let encoded = encode(image);
    let time_after = Instant::now();

    info!(
        "{}: lossy size {}K (parse: {} ms, decode: {} ms, resize: {} ms, encode: {} ms)",
        encoded.hash,
        encoded.data_webp.len() / 1024,
        (time_after_parse - time_before).whole_milliseconds(),
        (time_after_decode - time_after_parse).whole_milliseconds(),
        (time_after_resize - time_after_decode).whole_milliseconds(),
        (time_after - time_after_resize).whole_milliseconds(),
    );


    debug!(
        "processed image {}: {} bytes, {}x{} -> {} bytes, {}x{}",
        encoded.hash,
        data.len(),
        width,
        height,
        encoded.data_webp.len(),
        encoded.width,
        encoded.height
    );
    Ok(encoded)
}

fn reader_for(data: &[u8]) -> image::io::Reader<Cursor<&[u8]>> {
    image::io::Reader::new(Cursor::new(data))
        .with_guessed_format()
        .expect("cursor i/o is infallible")
}

#[instrument(skip_all)]
fn resize(image: DynamicImage, kind: ImageKind) -> DynamicImage {
    let (target_width, target_height) = kind.size();
    if image.width() <= target_width && image.height() <= target_height {
        // don't resize if already smaller
        return image;
    }

    // todo: best filter?
    let resized = image.resize(
        target_width,
        target_height,
        image::imageops::FilterType::Lanczos3,
    );
    return resized;
}

#[instrument(skip_all)]
// can't believe this is infallible
fn encode(image: DynamicImage) -> ProcessOutput {
    let (width, height) = (image.width(), image.height());
    let image_buf = image.to_rgba8();

    let encoded_lossy = webp::Encoder::new(&*image_buf, webp::PixelLayout::Rgba, width, height)
        .encode_simple(false, 90.0)
        .expect("encode should be infallible")
        .to_vec();

    let hash = Hash::sha256(&encoded_lossy);

    ProcessOutput {
        data_webp: encoded_lossy,
        hash,
        width,
        height,
    }
}
