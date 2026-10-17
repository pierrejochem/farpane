//! Encode and decode with the real `OpenH264` library. Skipped when it is not
//! installed: put Cisco's file in `$FARPANE_H264_DIR` (default
//! `target/h264-test`), or run `farpane-server h264 install --dir <dir>`.

use std::path::PathBuf;

use farpane_h264::OpenH264;
use rfb_codec::video::{VideoCodec, payload};

fn library() -> Option<OpenH264> {
    let dir = std::env::var_os("FARPANE_H264_DIR").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/h264-test"),
        PathBuf::from,
    );
    let found = OpenH264::open(&dir).expect("a file that is there loads");
    if found.is_none() {
        eprintln!("OpenH264 is not installed in {}: skipped", dir.display());
    }
    found
}

/// A picture with structure: gradients and a bright square at `x`.
fn picture(width: usize, height: usize, x: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        for col in 0..width {
            let in_square = (x..x + 16).contains(&col) && (8..24).contains(&row);
            let (b, g, r) = if in_square {
                (240, 240, 240)
            } else {
                (
                    u8::try_from(col * 255 / width).unwrap(),
                    u8::try_from(row * 255 / height).unwrap(),
                    90,
                )
            };
            out.extend_from_slice(&[b, g, r, 255]);
        }
    }
    out
}

/// How far apart two pictures are, as the mean absolute difference per byte.
fn mean_difference(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let total: u64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| u64::from(x.abs_diff(*y)))
        .sum();
    #[allow(clippy::cast_precision_loss)]
    let mean = total as f64 / a.len() as f64;
    mean
}

#[test]
fn a_stream_of_frames_comes_back_as_nearly_the_same_pictures() {
    let Some(codec) = library() else { return };
    let (w, h) = (96usize, 64usize);
    let mut encoder = codec.encoder(96, 64).unwrap();
    let mut decoder = codec.decoder().unwrap();
    let mut sizes = Vec::new();
    for (i, x) in [10usize, 14, 18, 22].into_iter().enumerate() {
        let source = picture(w, h, x);
        let data = encoder.encode(&source, 96, 64, w * 4, i == 0).unwrap();
        sizes.push(data.len());
        assert!(
            data.starts_with(&[0, 0, 0, 1]) || data.starts_with(&[0, 0, 1]),
            "Annex B"
        );
        let out = decoder.decode(&data).unwrap().expect("a picture");
        assert_eq!((usize::from(out.width), usize::from(out.height)), (w, h));
        let diff = mean_difference(&source, &out.bgrx);
        assert!(diff < 12.0, "frame {i}: mean difference {diff}");
        assert!(out.bgrx.chunks_exact(4).all(|p| p[3] == 255));
    }
    // Later frames carry only what changed.
    assert!(sizes[1] < sizes[0], "{sizes:?}");
}

#[test]
fn odd_sizes_are_padded_and_cropped_back_by_the_contexts() {
    let Some(codec) = library() else { return };
    let (w, h) = (95usize, 63usize);
    let source = picture(w, h, 20);
    let mut encoder = codec.encoder(95, 63).unwrap();
    let data = encoder.encode(&source, 95, 63, w * 4, true).unwrap();
    let mut contexts = rfb_codec::video::Contexts::new(std::sync::Arc::new(codec));
    let rect = rfb_proto::types::Rect {
        x: 0,
        y: 0,
        width: 95,
        height: 63,
    };
    let out = contexts
        .apply(rect, &payload(rfb_codec::video::RESET_CONTEXT, &data))
        .unwrap()
        .expect("a picture");
    assert_eq!(out.len(), w * h * 4);
    assert!(mean_difference(&source, &out) < 12.0);
}

#[test]
fn garbage_is_an_error_not_a_crash() {
    let Some(codec) = library() else { return };
    let mut decoder = codec.decoder().unwrap();
    // Not an H.264 stream: the decoder says no picture or an error.
    let result = decoder.decode(&[0, 0, 0, 1, 0x65, 1, 2, 3, 4, 5, 6, 7, 8]);
    assert!(!matches!(result, Ok(Some(_))), "{result:?}");
}
