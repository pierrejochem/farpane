# UI and icon mockups

These are the sources of the **Farpane Dashboards** design canvas:
https://claude.ai/artifact/9E9g4hrbyjJAuM9JuRDXVN

They are Design Component files (`.dc.html`) for that canvas, not standalone web pages: open the canvas link to view and click through them. They are kept here as the reference for the egui implementation.

| File | Screen |
|---|---|
| `Main.dc.html` | Viewer · Connections |
| `ViewerSession.dc.html` | Viewer · Live session |
| `ViewerFullscreen.dc.html` | Viewer · Fullscreen |
| `Server.dc.html` | Server · Dashboard |
| `Appearance.dc.html` | Settings · Appearance (themes and options) |
| `IconConcepts.dc.html`, `IconSystem.dc.html`, `Mark.dc.html` | Brand icon (concept 03 chosen) |
| `canvas.json` | Canvas layout |
| `glass-backdrop.svg` | The shattered-slab background of the Glass theme |

Design tokens for all themes are in the `<style>` block of each screen and mirrored in `crates/farpane-ui/src/theme.rs`.
