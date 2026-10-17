//! H.264 for Farpane through Cisco's `OpenH264` library.
//!
//! Farpane does not contain or link H.264 code. Cisco publishes `OpenH264` as a
//! prebuilt library and covers the patent licences for copies that people
//! download from Cisco, so the library is fetched once, on request
//! ([`install`]), checked against the hash Cisco published, and loaded at run
//! time ([`OpenH264::load`]). It then serves the viewer's decoder and the
//! server's encoder through [`rfb_codec::video::VideoCodec`].
//!
//! The pictures are converted between RGB and YUV with the matrix the
//! `openh264` crate uses on both ends, so a Farpane viewer and server agree;
//! other implementations may show slightly different colours.

#![allow(clippy::doc_markdown)] // OpenH264 is a name, not code

use std::{
    fmt,
    io::Read,
    path::{Path, PathBuf},
};

use openh264::{
    OpenH264API,
    decoder::{Decoder, DecoderConfig},
    encoder::{Encoder, EncoderConfig, Profile, UsageType},
    formats::{BgraSliceU8, YUVBuffer, YUVSource},
    nal_units,
};
use rfb_codec::video::{Picture, VideoCodec, VideoDecoder, VideoEncoder};
use sha2::{Digest, Sha256};

/// The `OpenH264` release this build knows the hash of.
pub const VERSION: &str = "2.6.0";

/// Where Cisco serves its binaries.
const DOWNLOAD_BASE: &str = "http://ciscobinary.openh264.org/";

/// The biggest download accepted (the real files are about 0.6 MB).
const MAX_DOWNLOAD: u64 = 16 * 1024 * 1024;

/// What can go wrong.
#[derive(Debug)]
pub enum Error {
    /// Cisco publishes no binary for this platform.
    UnsupportedPlatform,
    /// Reading or writing a file.
    Io(std::io::Error),
    /// The download failed.
    Download(String),
    /// What was downloaded is not the file Cisco published (the hash differs).
    HashMismatch { found: String },
    /// The library would not load or work.
    Library(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPlatform => {
                f.write_str("Cisco publishes no OpenH264 binary for this platform")
            }
            Self::Io(e) => write!(f, "{e}"),
            Self::Download(e) => write!(f, "download failed: {e}"),
            Self::HashMismatch { found } => {
                write!(
                    f,
                    "the file is not the one Cisco published (SHA-256 {found})"
                )
            }
            Self::Library(e) => write!(f, "OpenH264 cannot be used: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// The file Cisco publishes for this platform and its SHA-256 (the
/// uncompressed file), if any.
#[must_use]
pub fn platform_file() -> Option<(&'static str, &'static str)> {
    // Hashes of OpenH264 2.6.0, from Cisco's release (the same list the
    // `openh264` crate checks libraries against).
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some((
            "libopenh264-2.6.0-linux64.8.so",
            "2f0cde7c6a6abcf5cae76942894ea42897fa677bce4ed6c91a24dd1b041d5f04",
        ))
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some((
            "libopenh264-2.6.0-linux-arm64.8.so",
            "12e7b33623667cdab0e575170c147b1b36eadb77d0d2aa7ceb5afd3e58902140",
        ))
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some((
            "libopenh264-2.6.0-mac-arm64.dylib",
            "052e98bfcf7a9167d22f3bbb3f5988ef79065591f36af8b52924b22b13624551",
        ))
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some((
            "libopenh264-2.6.0-mac-x64.dylib",
            "e3dc8bc01fe69363f61fd3c02fd27798537a585eadd38cd808f303d1ee505a19",
        ))
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some((
            "openh264-2.6.0-win64.dll",
            "2076cb5675ec6c1a4c70e7a2a322552f547b6eeed649d6dfcd9e02a543b24691",
        ))
    } else {
        None
    }
}

/// The folder the library is kept in, inside the config folder.
#[must_use]
pub fn library_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("h264")
}

/// The installed library for this platform, if it is there.
#[must_use]
pub fn installed(dir: &Path) -> Option<PathBuf> {
    let (name, _) = platform_file()?;
    let path = dir.join(name);
    path.is_file().then_some(path)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// Download Cisco's `OpenH264` library for this platform into `dir`, check it
/// against the published hash and return its path. Replaces what is there.
///
/// This talks to `ciscobinary.openh264.org` over plain HTTP, as Cisco's
/// download address does; the hash check is what makes the file trustworthy.
///
/// # Errors
/// [`Error::UnsupportedPlatform`], network and file errors, and
/// [`Error::HashMismatch`] (nothing is kept then).
pub fn install(dir: &Path) -> Result<PathBuf, Error> {
    let (name, expected) = platform_file().ok_or(Error::UnsupportedPlatform)?;
    let url = format!("{DOWNLOAD_BASE}{name}.bz2");
    let mut response = ureq::get(&url)
        .call()
        .map_err(|e| Error::Download(e.to_string()))?;
    let mut compressed = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_DOWNLOAD)
        .read_to_end(&mut compressed)
        .map_err(|e| Error::Download(e.to_string()))?;
    let mut file = Vec::new();
    bzip2::read::BzDecoder::new(compressed.as_slice())
        .take(MAX_DOWNLOAD)
        .read_to_end(&mut file)
        .map_err(|e| Error::Download(format!("cannot unpack the file: {e}")))?;
    let found = sha256_hex(&file);
    if found != expected {
        return Err(Error::HashMismatch { found });
    }
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    let tmp = dir.join(format!("{name}.part"));
    std::fs::write(&tmp, &file)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// A download running on its own thread, for windows that must not freeze.
#[derive(Debug)]
pub struct Installer {
    result: std::sync::Arc<std::sync::Mutex<Option<Result<PathBuf, String>>>>,
}

impl Installer {
    /// Start downloading into `dir`.
    #[must_use]
    pub fn start(dir: PathBuf) -> Self {
        let result = std::sync::Arc::new(std::sync::Mutex::new(None));
        let slot = std::sync::Arc::clone(&result);
        std::thread::spawn(move || {
            let outcome = install(&dir).map_err(|e| e.to_string());
            *slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(outcome);
        });
        Self { result }
    }

    /// The outcome, once there is one: the installed path, or why not.
    #[must_use]
    pub fn finished(&self) -> Option<Result<PathBuf, String>> {
        self.result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// What to tell a person before the library is downloaded.
pub const NOTICE: &str = "OpenH264 is Cisco's H.264 library. Farpane does not contain it: it is \
downloaded from Cisco (ciscobinary.openh264.org), checked against the hash Cisco published, and \
used under Cisco's OpenH264 licence, which includes the H.264 patent licences for copies obtained \
this way.";

/// A loaded `OpenH264` library.
#[derive(Clone)]
pub struct OpenH264 {
    path: PathBuf,
}

impl fmt::Debug for OpenH264 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenH264")
            .field("path", &self.path)
            .finish()
    }
}

impl OpenH264 {
    /// Check that the library at `path` is one of Cisco's published files and
    /// loads.
    ///
    /// # Errors
    /// [`Error::Library`] when it is not, or does not load.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let this = Self {
            path: path.to_owned(),
        };
        this.api()?;
        Ok(this)
    }

    /// The library in `dir`, when it is installed.
    ///
    /// # Errors
    /// [`Error::Library`] when a file is there but unusable.
    pub fn open(dir: &Path) -> Result<Option<Self>, Error> {
        installed(dir).map(|p| Self::load(&p)).transpose()
    }

    /// Each decoder and encoder gets its own handle on the library. Loading
    /// the same file again only counts a reference, and the hash check keeps
    /// a swapped file from being loaded later.
    fn api(&self) -> Result<OpenH264API, Error> {
        OpenH264API::from_blob_path(&self.path).map_err(|e| Error::Library(e.to_string()))
    }
}

struct Dec(Decoder);

impl fmt::Debug for Dec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Dec")
    }
}

impl VideoDecoder for Dec {
    fn decode(&mut self, data: &[u8]) -> Result<Option<Picture>, String> {
        let mut picture = None;
        for nal in nal_units(data) {
            if let Some(yuv) = self.0.decode(nal).map_err(|e| e.to_string())? {
                let (width, height) = yuv.dimensions();
                let mut rgba = vec![0u8; width * height * 4];
                yuv.write_rgba8(&mut rgba);
                // The framebuffer wants B, G, R, X.
                for px in rgba.chunks_exact_mut(4) {
                    px.swap(0, 2);
                    px[3] = 255;
                }
                picture = Some(Picture {
                    width: u16::try_from(width).map_err(|_| "picture too wide")?,
                    height: u16::try_from(height).map_err(|_| "picture too tall")?,
                    bgrx: rgba,
                });
            }
        }
        Ok(picture)
    }
}

struct Enc(Encoder);

impl fmt::Debug for Enc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Enc")
    }
}

impl VideoEncoder for Enc {
    fn encode(
        &mut self,
        bgrx: &[u8],
        width: u16,
        height: u16,
        stride: usize,
        key: bool,
    ) -> Result<Vec<u8>, String> {
        let (w, h) = (usize::from(width), usize::from(height));
        if w == 0 || h == 0 || bgrx.len() < stride * (h - 1) + w * 4 {
            return Err("picture does not fit its size".into());
        }
        // The encoder wants even sizes: repeat the last column and row.
        let (ew, eh) = (w + w % 2, h + h % 2);
        let mut padded = Vec::with_capacity(ew * eh * 4);
        for y in 0..eh {
            let row = &bgrx[y.min(h - 1) * stride..][..w * 4];
            padded.extend_from_slice(row);
            if ew > w {
                padded.extend_from_slice(&row[(w - 1) * 4..]);
            }
        }
        let yuv = YUVBuffer::from_bgra8_source(BgraSliceU8::new(&padded, (ew, eh)));
        if key {
            self.0.force_intra_frame();
        }
        let stream = self.0.encode(&yuv).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        stream.write_vec(&mut out);
        Ok(out)
    }
}

impl VideoCodec for OpenH264 {
    fn decoder(&self) -> Result<Box<dyn VideoDecoder>, String> {
        let api = self.api().map_err(|e| e.to_string())?;
        Decoder::with_api_config(api, DecoderConfig::new())
            .map(|d| Box::new(Dec(d)) as Box<dyn VideoDecoder>)
            .map_err(|e| e.to_string())
    }

    fn encoder(&self, _width: u16, _height: u16) -> Result<Box<dyn VideoEncoder>, String> {
        let api = self.api().map_err(|e| e.to_string())?;
        // Real-time screen content, Constrained Baseline: what the Open H.264
        // encoding asks for, and what a decoder of any age can read.
        let config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .profile(Profile::Baseline);
        Encoder::with_api_config(api, config)
            .map(|e| Box::new(Enc(e)) as Box<dyn VideoEncoder>)
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_platform_has_a_known_file_and_hash() {
        if let Some((name, hash)) = platform_file() {
            assert!(name.contains(VERSION), "{name}");
            assert_eq!(hash.len(), 64);
            assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn a_missing_library_is_not_installed_and_a_wrong_file_is_refused() {
        let dir = std::env::temp_dir().join(format!("farpane-h264-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(installed(&dir).is_none());
        assert!(OpenH264::open(&dir).unwrap().is_none());
        if let Some((name, _)) = platform_file() {
            std::fs::write(dir.join(name), b"not a library").unwrap();
            // The file is there, but it is not Cisco's: refused before loading.
            assert!(matches!(OpenH264::open(&dir), Err(Error::Library(_))));
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_hash_helper_matches_a_known_value() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
