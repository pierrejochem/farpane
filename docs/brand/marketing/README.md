# Farpane marketing package

| File | What |
|---|---|
| `brand-guidelines.md` | Positioning, voice, claims policy, colour, type, logo use |
| `copy.md` | Taglines, boilerplate (25, 50, 100 words), feature lines, launch posts, release notes template, FAQ |
| `press-kit.md` | Fact sheet, platform table, trademark note, asset list |
| `assets/` | Rendered images: GitHub social preview, Open Graph, X card and header, LinkedIn banner, square |
| `src/` | HTML sources for the images (`card.html`, `banner.html`, shared `common.css`, `panes.svg`) |
| `render.py` | Renders `src/` to `assets/` with headless Chrome |

Render: `python3 docs/brand/marketing/render.py [name ...]`. Without names it
renders everything and copies the Open Graph card to `site/static/og.png`.

GitHub does not let a tool set the repository's social preview. Upload
`assets/github-social-preview.png` by hand: Settings, General, Social preview.

Check the claims policy in `brand-guidelines.md` before publishing anything
that quotes these files, and keep the status line while Farpane is unreleased.
