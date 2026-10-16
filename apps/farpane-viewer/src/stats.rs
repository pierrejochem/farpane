//! Connection statistics for the overlay: update rate, bandwidth and the mix
//! of encodings the server uses.
//!
//! Round-trip time comes only from the Fence extension (a `ClientFence`
//! that blocks before, answered once the server has processed everything we
//! sent). A figure derived from update requests would mostly measure how
//! long the screen stayed unchanged, so a server without Fence shows none.

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use rfb_codec::enc;
use rfb_proto::consts::pseudo;

/// What one encoding contributed since the connection started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodingShare {
    pub encoding: i32,
    pub rects: u64,
    /// Payload bytes (before decoding), without rectangle headers.
    pub bytes: u64,
}

/// One reading, taken a few times per second.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// Completed `FramebufferUpdate` messages per second since the last reading.
    pub updates_per_sec: f64,
    /// Bytes per second on the wire since the last reading.
    pub rx_per_sec: f64,
    pub tx_per_sec: f64,
    pub total_rx: u64,
    pub total_tx: u64,
    /// Encodings seen, the one that carried the most bytes first.
    pub encodings: Vec<EncodingShare>,
    /// The latest Fence round trip, if the server supports Fence and has
    /// answered at least once.
    pub rtt: Option<Duration>,
    /// The server takes part in the Extended Clipboard extension (UTF-8 text).
    pub ext_clipboard: bool,
}

/// Accumulates counts between readings.
#[derive(Debug)]
pub struct Collector {
    last_at: Instant,
    last_rx: u64,
    last_tx: u64,
    updates: u32,
    by_encoding: BTreeMap<i32, (u64, u64)>,
    rtt: Option<Duration>,
}

impl Collector {
    #[must_use]
    pub fn new(now: Instant) -> Self {
        Self {
            last_at: now,
            last_rx: 0,
            last_tx: 0,
            updates: 0,
            by_encoding: BTreeMap::new(),
            rtt: None,
        }
    }

    /// A `FramebufferUpdate` finished.
    pub fn record_update(&mut self) {
        self.updates = self.updates.saturating_add(1);
    }

    /// A Fence answer arrived `rtt` after its request.
    pub fn record_rtt(&mut self, rtt: Duration) {
        self.rtt = Some(rtt);
    }

    /// A rectangle with `payload_bytes` of encoded data arrived.
    pub fn record_rect(&mut self, encoding: i32, payload_bytes: usize) {
        let entry = self.by_encoding.entry(encoding).or_default();
        entry.0 += 1;
        entry.1 += payload_bytes as u64;
    }

    /// Take a reading at `now`, given the stream's running byte totals.
    pub fn sample(&mut self, now: Instant, total_rx: u64, total_tx: u64) -> Snapshot {
        let secs = now.saturating_duration_since(self.last_at).as_secs_f64();
        // Byte counts stay far below 2^52, where `f64` is exact.
        #[allow(clippy::cast_precision_loss)]
        let per_sec = |delta: u64| if secs > 0.0 { delta as f64 / secs } else { 0.0 };
        let snapshot = Snapshot {
            updates_per_sec: per_sec(u64::from(self.updates)),
            rx_per_sec: per_sec(total_rx.saturating_sub(self.last_rx)),
            tx_per_sec: per_sec(total_tx.saturating_sub(self.last_tx)),
            total_rx,
            total_tx,
            encodings: self.shares(),
            rtt: self.rtt,
            ext_clipboard: false,
        };
        self.last_at = now;
        self.last_rx = total_rx;
        self.last_tx = total_tx;
        self.updates = 0;
        snapshot
    }

    fn shares(&self) -> Vec<EncodingShare> {
        let mut shares: Vec<EncodingShare> = self
            .by_encoding
            .iter()
            .map(|(&encoding, &(rects, bytes))| EncodingShare {
                encoding,
                rects,
                bytes,
            })
            .collect();
        shares.sort_by_key(|s| std::cmp::Reverse((s.bytes, s.rects)));
        shares
    }
}

/// The pixel encodings' shares of the received payload as whole percentages,
/// largest first, at most `limit` of them. Pseudo-encodings (cursor shape,
/// desktop size: negative numbers) carry no pixels and are left out.
#[must_use]
pub fn pixel_mix(snapshot: &Snapshot, limit: usize) -> Vec<(i32, u32)> {
    let pixels: Vec<&EncodingShare> = snapshot
        .encodings
        .iter()
        .filter(|e| e.encoding >= 0)
        .collect();
    let total: u64 = pixels.iter().map(|e| e.bytes).sum();
    if total == 0 {
        return Vec::new();
    }
    pixels
        .into_iter()
        .take(limit)
        .map(|e| {
            // 0..=100, so the conversion is exact.
            let percent = (e.bytes * 100 + total / 2) / total;
            (e.encoding, u32::try_from(percent).unwrap_or(100))
        })
        .collect()
}

/// A human name for an encoding number.
#[must_use]
pub fn encoding_name(encoding: i32) -> String {
    match encoding {
        enc::RAW => "Raw".into(),
        enc::COPY_RECT => "CopyRect".into(),
        enc::RRE => "RRE".into(),
        enc::CO_RRE => "CoRRE".into(),
        enc::HEXTILE => "Hextile".into(),
        enc::ZLIB => "zlib".into(),
        enc::TIGHT => "Tight".into(),
        enc::TRLE => "TRLE".into(),
        enc::ZRLE => "ZRLE".into(),
        pseudo::CURSOR => "Cursor".into(),
        pseudo::DESKTOP_SIZE => "DesktopSize".into(),
        other => format!("#{other}"),
    }
}

/// The round-trip time for the overlay: `12 ms`, or `rtt n/a` when the
/// server has not answered a Fence request (it may lack the extension).
#[must_use]
pub fn format_rtt(rtt: Option<Duration>) -> String {
    match rtt {
        Some(d) if d < Duration::from_millis(1) => "rtt <1 ms".to_owned(),
        Some(d) => format!("rtt {} ms", d.as_millis()),
        None => "rtt n/a".to_owned(),
    }
}

/// "850 B/s", "12.3 KB/s", "4.56 MB/s" (decimal units, as network rates are
/// usually quoted).
#[must_use]
pub fn format_rate(bytes_per_sec: f64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec))
}

/// "850 B", "12.3 KB", "4.56 MB", "1.20 GB".
#[must_use]
pub fn format_bytes(bytes: f64) -> String {
    const KB: f64 = 1000.0;
    if bytes < KB {
        format!("{bytes:.0} B")
    } else if bytes < KB * KB {
        format!("{:.1} KB", bytes / KB)
    } else if bytes < KB * KB * KB {
        format!("{:.2} MB", bytes / (KB * KB))
    } else {
        format!("{:.2} GB", bytes / (KB * KB * KB))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn at(base: Instant, secs: f64) -> Instant {
        base + Duration::from_secs_f64(secs)
    }

    #[test]
    fn rates_are_deltas_over_the_interval() {
        let t0 = Instant::now();
        let mut c = Collector::new(t0);
        for _ in 0..30 {
            c.record_update();
        }
        let a = c.sample(at(t0, 0.5), 50_000, 100);
        assert!((a.updates_per_sec - 60.0).abs() < 1e-9);
        assert!((a.rx_per_sec - 100_000.0).abs() < 1e-6);
        assert!((a.tx_per_sec - 200.0).abs() < 1e-9);
        assert_eq!((a.total_rx, a.total_tx), (50_000, 100));

        // The next reading covers only what happened since the last one.
        for _ in 0..5 {
            c.record_update();
        }
        let b = c.sample(at(t0, 1.5), 60_000, 100);
        assert!((b.updates_per_sec - 5.0).abs() < 1e-9);
        assert!((b.rx_per_sec - 10_000.0).abs() < 1e-6);
        assert!(b.tx_per_sec.abs() < 1e-9);
    }

    #[test]
    fn a_reading_with_no_time_elapsed_does_not_divide_by_zero() {
        let t0 = Instant::now();
        let mut c = Collector::new(t0);
        c.record_update();
        let s = c.sample(t0, 1000, 10);
        assert_eq!(
            (s.updates_per_sec, s.rx_per_sec, s.tx_per_sec),
            (0.0, 0.0, 0.0)
        );
        assert_eq!(s.total_rx, 1000);
    }

    #[test]
    fn counters_that_go_backwards_are_treated_as_no_traffic() {
        let t0 = Instant::now();
        let mut c = Collector::new(t0);
        c.sample(at(t0, 1.0), 5000, 5000);
        let s = c.sample(at(t0, 2.0), 100, 100);
        assert_eq!((s.rx_per_sec, s.tx_per_sec), (0.0, 0.0));
    }

    #[test]
    fn the_encoding_mix_is_cumulative_and_sorted_by_bytes() {
        let t0 = Instant::now();
        let mut c = Collector::new(t0);
        c.record_rect(enc::ZRLE, 1000);
        c.record_rect(enc::ZRLE, 500);
        c.record_rect(enc::COPY_RECT, 4);
        c.record_rect(enc::RAW, 9000);
        let first = c.sample(at(t0, 1.0), 0, 0);
        assert_eq!(
            first.encodings,
            [
                EncodingShare {
                    encoding: enc::RAW,
                    rects: 1,
                    bytes: 9000
                },
                EncodingShare {
                    encoding: enc::ZRLE,
                    rects: 2,
                    bytes: 1500
                },
                EncodingShare {
                    encoding: enc::COPY_RECT,
                    rects: 1,
                    bytes: 4
                },
            ]
        );
        // Not reset between readings.
        c.record_rect(enc::COPY_RECT, 4);
        let second = c.sample(at(t0, 2.0), 0, 0);
        assert_eq!(second.encodings.last().unwrap().rects, 2);
    }

    #[test]
    fn the_pixel_mix_leaves_out_pseudo_encodings_and_rounds_to_percent() {
        let snap = |encodings: Vec<EncodingShare>| Snapshot {
            updates_per_sec: 0.0,
            rx_per_sec: 0.0,
            tx_per_sec: 0.0,
            total_rx: 0,
            total_tx: 0,
            encodings,
            rtt: None,
            ext_clipboard: false,
        };
        let share = |encoding, bytes| EncodingShare {
            encoding,
            rects: 1,
            bytes,
        };
        let s = snap(vec![
            share(enc::ZRLE, 7500),
            share(enc::HEXTILE, 2000),
            share(enc::RAW, 500),
            share(pseudo::CURSOR, 99_999), // not pixels: ignored however large
        ]);
        assert_eq!(
            pixel_mix(&s, 8),
            [(enc::ZRLE, 75), (enc::HEXTILE, 20), (enc::RAW, 5)]
        );
        assert_eq!(pixel_mix(&s, 2), [(enc::ZRLE, 75), (enc::HEXTILE, 20)]);
        assert_eq!(pixel_mix(&snap(vec![]), 3), []);
        assert_eq!(pixel_mix(&snap(vec![share(pseudo::CURSOR, 10)]), 3), []);
        assert_eq!(
            pixel_mix(&snap(vec![share(enc::RAW, 0)]), 3),
            [],
            "no bytes, no percentages"
        );
    }

    #[test]
    fn names_and_units_read_naturally() {
        assert_eq!(encoding_name(enc::ZRLE), "ZRLE");
        assert_eq!(encoding_name(enc::HEXTILE), "Hextile");
        assert_eq!(encoding_name(pseudo::CURSOR), "Cursor");
        assert_eq!(encoding_name(-260), "#-260");
        assert_eq!(format_bytes(0.0), "0 B");
        assert_eq!(format_bytes(999.0), "999 B");
        assert_eq!(format_bytes(1000.0), "1.0 KB");
        assert_eq!(format_bytes(12_345.0), "12.3 KB");
        assert_eq!(format_bytes(4_560_000.0), "4.56 MB");
        assert_eq!(format_bytes(1_200_000_000.0), "1.20 GB");
        assert_eq!(format_rate(850.0), "850 B/s");
    }
}
