//! Protocol numbers.
//!
//! Verified on 2026-10-15 against the IANA "Remote Framebuffer (RFB)" registry
//! (updated 2024-08-14), the community specification (rfbproto.rst) and
//! noVNC's `encodings.js`. Encoding numbers are signed 32-bit values on the
//! wire.

/// Security types (`U8` in RFB 3.7+, `U32` in 3.3).
pub mod security {
    pub const INVALID: u8 = 0;
    pub const NONE: u8 = 1;
    pub const VNC_AUTH: u8 = 2;
    pub const RA2: u8 = 5;
    pub const RA2NE: u8 = 6;
    pub const RA2R: u8 = 13;
    pub const TIGHT: u8 = 16;
    pub const VENCRYPT: u8 = 19;
    pub const SASL: u8 = 20;
    pub const XVP: u8 = 22;
    pub const APPLE_DH: u8 = 30;
    pub const MSLOGON_II: u8 = 113;
    pub const RA2_256: u8 = 129;
    pub const RA2NE_256: u8 = 130;
    pub const RA2R_256: u8 = 133;
}

/// `VeNCrypt` (security type 19) subtypes, sent as `U32`.
pub mod vencrypt {
    pub const PLAIN: u32 = 256;
    pub const TLS_NONE: u32 = 257;
    pub const TLS_VNC: u32 = 258;
    pub const TLS_PLAIN: u32 = 259;
    pub const X509_NONE: u32 = 260;
    pub const X509_VNC: u32 = 261;
    pub const X509_PLAIN: u32 = 262;
    pub const TLS_SASL: u32 = 263;
    pub const X509_SASL: u32 = 264;
    pub const IDENT: u32 = 265;
    pub const TLS_IDENT: u32 = 266;
    pub const X509_IDENT: u32 = 267;
}

/// Client-to-server message types.
pub mod client_msg {
    pub const SET_PIXEL_FORMAT: u8 = 0;
    pub const SET_ENCODINGS: u8 = 2;
    pub const FRAMEBUFFER_UPDATE_REQUEST: u8 = 3;
    pub const KEY_EVENT: u8 = 4;
    pub const POINTER_EVENT: u8 = 5;
    pub const CLIENT_CUT_TEXT: u8 = 6;
    pub const ENABLE_CONTINUOUS_UPDATES: u8 = 150;
    pub const CLIENT_FENCE: u8 = 248;
    pub const XVP: u8 = 250;
    pub const SET_DESKTOP_SIZE: u8 = 251;
    pub const GII: u8 = 253;
    pub const QEMU: u8 = 255;
}

/// Server-to-client message types.
pub mod server_msg {
    pub const FRAMEBUFFER_UPDATE: u8 = 0;
    pub const SET_COLOUR_MAP_ENTRIES: u8 = 1;
    pub const BELL: u8 = 2;
    pub const SERVER_CUT_TEXT: u8 = 3;
    pub const END_OF_CONTINUOUS_UPDATES: u8 = 150;
    pub const SERVER_FENCE: u8 = 248;
    pub const XVP: u8 = 250;
    pub const GII: u8 = 253;
    pub const QEMU: u8 = 255;
}

/// Pixel encodings.
pub mod enc {
    pub const RAW: i32 = 0;
    pub const COPY_RECT: i32 = 1;
    pub const RRE: i32 = 2;
    pub const CO_RRE: i32 = 4;
    pub const HEXTILE: i32 = 5;
    pub const ZLIB: i32 = 6;
    pub const TIGHT: i32 = 7;
    pub const ZLIB_HEX: i32 = 8;
    pub const TRLE: i32 = 15;
    pub const ZRLE: i32 = 16;
    pub const ZYWRLE: i32 = 17;
    pub const JPEG: i32 = 21;
    pub const OPEN_H264: i32 = 50;
    pub const TIGHT_PNG: i32 = -260;
}

/// Fence flags (rfbproto.rst, "`ClientFence`").
pub mod fence {
    pub const BLOCK_BEFORE: u32 = 1 << 0;
    pub const BLOCK_AFTER: u32 = 1 << 1;
    pub const SYNC_NEXT: u32 = 1 << 2;
    /// Set on a request, cleared on the response.
    pub const REQUEST: u32 = 1 << 31;
}

/// Pseudo-encodings (capability flags announced in `SetEncodings`).
pub mod pseudo {
    pub const JPEG_QUALITY_9: i32 = -23;
    pub const JPEG_QUALITY_0: i32 = -32;
    pub const DESKTOP_SIZE: i32 = -223;
    pub const LAST_RECT: i32 = -224;
    pub const POINTER_POS: i32 = -232;
    pub const CURSOR: i32 = -239;
    pub const X_CURSOR: i32 = -240;
    pub const COMPRESS_9: i32 = -247;
    pub const COMPRESS_0: i32 = -256;
    pub const QEMU_POINTER_MOTION: i32 = -257;
    pub const QEMU_EXT_KEY: i32 = -258;
    pub const QEMU_AUDIO: i32 = -259;
    pub const QEMU_LED: i32 = -261;
    pub const GII: i32 = -305;
    pub const DESKTOP_NAME: i32 = -307;
    pub const EXT_DESKTOP_SIZE: i32 = -308;
    pub const XVP: i32 = -309;
    pub const CLIENT_REDIRECT: i32 = -311;
    pub const FENCE: i32 = -312;
    pub const CONTINUOUS_UPDATES: i32 = -313;
    pub const CURSOR_WITH_ALPHA: i32 = -314;
    pub const COLOR_MAP: i32 = -315;
    pub const EXT_MOUSE_BUTTONS: i32 = -316;
    pub const TIGHT_NO_ZLIB: i32 = -317;
    pub const FINE_QUALITY_100: i32 = -412;
    pub const FINE_QUALITY_0: i32 = -512;
    pub const SUBSAMP_LAST: i32 = -763;
    pub const SUBSAMP_FIRST: i32 = -768;
    pub const VMWARE_CURSOR: i32 = 0x574D_5664;
    /// 0xC0A1E5CE does not fit an `i32`, so it is negative on the wire.
    #[allow(clippy::cast_possible_wrap)]
    pub const EXT_CLIPBOARD: i32 = 0xC0A1_E5CE_u32 as i32;
}

/// Hard limits applied while parsing. All are configurable later; these are
/// the defaults from the design doc (section 15, "Limits").
pub mod limits {
    pub const MAX_FB_DIMENSION: u16 = 16_384;
    pub const MAX_ENCODINGS: u16 = 1_024;
    pub const MAX_NAME_LEN: u32 = 64 * 1024;
    pub const MAX_CUT_TEXT_LEN: u32 = 1024 * 1024;
    pub const MAX_EXT_CLIPBOARD_INFLATED: u32 = 16 * 1024 * 1024;
    pub const MAX_FENCE_PAYLOAD: u8 = 64;
    pub const MAX_SCREENS: u8 = 255;
    /// Longest username or password accepted in `VeNCrypt` `X509Plain`.
    /// Not from the spec; real credentials are far shorter.
    pub const MAX_CREDENTIAL_LEN: u32 = 4096;
    /// Largest single rectangle payload the framers will wait for. Bounds
    /// buffering when a peer announces a huge Raw or length-prefixed rect.
    /// Not from the spec; a Raw 5120 x 5120 x 32 bpp rect (100 MB) fits.
    pub const MAX_RECT_PAYLOAD: u32 = 128 * 1024 * 1024;
    /// Largest H.264 data block (rfbproto, "Open H.264 Encoding") accepted.
    /// Not from the spec; a frame is far smaller, even an intra frame of an
    /// 8K screen.
    pub const MAX_H264_DATA: u32 = 32 * 1024 * 1024;
}

#[cfg(test)]
mod tests {
    use super::pseudo;

    #[test]
    fn extended_clipboard_round_trips_as_u32() {
        #[allow(clippy::cast_sign_loss)]
        let raw = pseudo::EXT_CLIPBOARD as u32;
        assert_eq!(raw, 0xC0A1_E5CE);
    }
}
