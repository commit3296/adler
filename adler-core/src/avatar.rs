//! Privacy-safe avatar perceptual hashing helpers.
//!
//! Adler never stores raw avatar bytes. The fetch helper reads a bounded
//! image response, computes a deterministic 64-bit difference hash, and returns
//! only the normalized hash string.

use std::time::Duration;

use image::imageops::FilterType;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE};
use thiserror::Error;

/// Stable prefix for Adler's current avatar perceptual hash algorithm.
pub const AVATAR_HASH_ALGORITHM: &str = "dhash64_v1";

/// Default maximum response body size for avatar hash fetches.
pub const DEFAULT_AVATAR_HASH_MAX_BYTES: usize = 1_000_000;

/// Default request timeout for avatar hash fetches.
pub const DEFAULT_AVATAR_HASH_TIMEOUT: Duration = Duration::from_secs(5);

/// Bounded fetch/decode options for avatar hashing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AvatarHashOptions {
    /// Maximum decoded HTTP response body bytes Adler will read.
    pub max_bytes: usize,
    /// Per-request timeout applied to the avatar image fetch.
    pub timeout: Duration,
}

impl Default for AvatarHashOptions {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_AVATAR_HASH_MAX_BYTES,
            timeout: DEFAULT_AVATAR_HASH_TIMEOUT,
        }
    }
}

/// Error returned while fetching or hashing an avatar image.
#[derive(Debug, Error)]
pub enum AvatarHashError {
    /// Avatar URL could not be parsed.
    #[error("invalid avatar URL: {0}")]
    InvalidUrl(#[from] url::ParseError),
    /// Only HTTP(S) avatar fetches are supported.
    #[error("avatar URL must use http or https")]
    UnsupportedScheme,
    /// The server returned a non-success HTTP status.
    #[error("avatar response was not successful: {0}")]
    Status(reqwest::StatusCode),
    /// Response content type was absent or outside the image allowlist.
    #[error("avatar response content type {0:?} is not allowed")]
    UnsupportedContentType(Option<String>),
    /// Response advertised or exceeded the configured size limit.
    #[error("avatar response exceeded {max_bytes} bytes")]
    TooLarge {
        /// Maximum allowed response size.
        max_bytes: usize,
    },
    /// HTTP client error.
    #[error("avatar fetch failed: {0}")]
    Request(#[from] reqwest::Error),
    /// Image decoder error.
    #[error("avatar image decode failed: {0}")]
    Decode(#[from] image::ImageError),
}

/// Fetch an avatar image through a caller-configured client and return its
/// normalized perceptual hash.
///
/// Redirect policy, proxying, and TLS policy come from the supplied
/// `reqwest::Client`. This helper adds URL scheme validation, content-type
/// allowlisting, timeout, and body-size enforcement.
pub async fn fetch_avatar_hash(
    client: &reqwest::Client,
    url: &str,
    options: AvatarHashOptions,
) -> Result<String, AvatarHashError> {
    let parsed = url::Url::parse(url)?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(AvatarHashError::UnsupportedScheme);
    }

    let mut response = client.get(parsed).timeout(options.timeout).send().await?;
    let status = response.status();
    if !status.is_success() {
        return Err(AvatarHashError::Status(status));
    }

    let content_type = normalized_content_type(response.headers().get(CONTENT_TYPE));
    if !content_type
        .as_deref()
        .is_some_and(allowed_avatar_content_type)
    {
        return Err(AvatarHashError::UnsupportedContentType(content_type));
    }

    if advertised_length(response.headers().get(CONTENT_LENGTH))
        .is_some_and(|length| length > options.max_bytes)
    {
        return Err(AvatarHashError::TooLarge {
            max_bytes: options.max_bytes,
        });
    }

    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let next_len = bytes.len().saturating_add(chunk.len());
        if next_len > options.max_bytes {
            return Err(AvatarHashError::TooLarge {
                max_bytes: options.max_bytes,
            });
        }
        bytes.extend_from_slice(&chunk);
    }

    avatar_hash_from_bytes(&bytes)
}

/// Compute Adler's normalized avatar perceptual hash from already-fetched
/// image bytes.
pub fn avatar_hash_from_bytes(bytes: &[u8]) -> Result<String, AvatarHashError> {
    let image = image::load_from_memory(bytes)?;
    let gray = image.resize_exact(9, 8, FilterType::Triangle).to_luma8();

    let mut bits = 0_u64;
    for y in 0..8 {
        for x in 0..8 {
            let left = gray.get_pixel(x, y)[0];
            let right = gray.get_pixel(x + 1, y)[0];
            let index = y * 8 + x;
            if left > right {
                bits |= 1_u64 << index;
            }
        }
    }

    Ok(format!("{AVATAR_HASH_ALGORITHM}:{bits:016x}"))
}

fn normalized_content_type(value: Option<&reqwest::header::HeaderValue>) -> Option<String> {
    value
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(';').next().unwrap_or(value).trim())
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
}

fn allowed_avatar_content_type(content_type: &str) -> bool {
    matches!(
        content_type,
        "image/gif" | "image/jpeg" | "image/png" | "image/webp"
    )
}

fn advertised_length(value: Option<&reqwest::header::HeaderValue>) -> Option<usize> {
    value
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn png_bytes(pattern: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        let image = RgbImage::from_fn(16, 16, |x, y| Rgb(pattern(x, y)));
        let mut cursor = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(image)
            .write_to(&mut cursor, ImageFormat::Png)
            .unwrap();
        cursor.into_inner()
    }

    #[test]
    fn same_image_produces_same_hash() {
        let image = png_bytes(|x, y| {
            if (x + y) % 2 == 0 {
                [255, 255, 255]
            } else {
                [0, 0, 0]
            }
        });

        let left = avatar_hash_from_bytes(&image).unwrap();
        let right = avatar_hash_from_bytes(&image).unwrap();

        assert_eq!(left, right);
        assert!(left.starts_with("dhash64_v1:"));
    }

    #[test]
    fn visibly_different_images_produce_different_hashes() {
        let vertical = png_bytes(|x, _| if x < 8 { [255, 255, 255] } else { [0, 0, 0] });
        let horizontal = png_bytes(|_, y| if y < 8 { [255, 255, 255] } else { [0, 0, 0] });

        let vertical_hash = avatar_hash_from_bytes(&vertical).unwrap();
        let horizontal_hash = avatar_hash_from_bytes(&horizontal).unwrap();

        assert_ne!(vertical_hash, horizontal_hash);
    }

    #[tokio::test]
    async fn fetch_rejects_non_image_responses() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/avatar.txt"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/plain")
                    .set_body_string("not an image"),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let err = fetch_avatar_hash(
            &client,
            &format!("{}/avatar.txt", server.uri()),
            AvatarHashOptions::default(),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, AvatarHashError::UnsupportedContentType(_)));
    }

    #[tokio::test]
    async fn fetch_rejects_oversized_responses_before_decoding() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/avatar.png"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .insert_header("content-length", "100")
                    .set_body_bytes(vec![0_u8; 100]),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let err = fetch_avatar_hash(
            &client,
            &format!("{}/avatar.png", server.uri()),
            AvatarHashOptions {
                max_bytes: 16,
                timeout: Duration::from_secs(1),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(err, AvatarHashError::TooLarge { max_bytes: 16 }));
    }
}
