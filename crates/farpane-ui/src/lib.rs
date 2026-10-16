//! Shared UI for Farpane Viewer and Farpane Server.
//!
//! The UI will be built with egui/eframe on wgpu (design doc section 16). This
//! crate holds the design tokens first, so widgets only ever read tokens and
//! never raw colours. Values match the mockups on the "Farpane Dashboards"
//! canvas (see `docs/design/mockups/`).

pub mod style;
pub mod theme;

pub use theme::{Accent, Theme, ThemePreference, Tokens};
