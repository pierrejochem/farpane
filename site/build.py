#!/usr/bin/env python3
"""Build the Farpane site (landing page and handbook) into _site/.

Static HTML, no external requests. Every page gets a title, description,
canonical URL, Open Graph and Twitter tags and JSON-LD; the build also writes
sitemap.xml, robots.txt and a 404 page.

Usage: python3 site/build.py   (needs the `markdown` package)
"""
import datetime
import html
import json
import pathlib
import re
import shutil
import subprocess

import markdown

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "_site"
BASE = "https://pierrejochem.github.io/farpane/"
REPO = "https://github.com/pierrejochem/farpane"
SITE_NAME = "Farpane"
TAGLINE = "VNC-compatible remote desktop viewer and server, written in Rust"
LANDING_TITLE = "Farpane: VNC-compatible remote desktop in Rust"
LANDING_DESC = (
    "Free, open-source VNC-compatible remote desktop viewer and server in Rust: "
    "TLS and RSA-AES encryption, X11 and Wayland sharing, user accounts, dashboard."
)


def today():
    try:
        return subprocess.check_output(
            ["git", "log", "-1", "--format=%cs"], cwd=ROOT, text=True
        ).strip()
    except Exception:
        return datetime.date.today().isoformat()


def last_modified(path):
    try:
        out = subprocess.check_output(
            ["git", "log", "-1", "--format=%cs", "--", str(path)], cwd=ROOT, text=True
        ).strip()
        return out or today()
    except Exception:
        return today()


def front_matter(text):
    m = re.match(r"---\n(.*?)\n---\n", text, re.S)
    meta = {}
    for line in m.group(1).splitlines():
        k, v = line.split(":", 1)
        meta[k.strip()] = v.strip()
    return meta, text[m.end():]


def esc(s):
    return html.escape(s, quote=True)


def head(title, description, url, kind="website", json_ld=None, noindex=False, bare=False):
    full = title if bare else f"{title} - {SITE_NAME}"
    ld = ""
    for block in json_ld or []:
        ld += f'<script type="application/ld+json">{json.dumps(block, separators=(",", ":"))}</script>\n'
    robots = '<meta name="robots" content="noindex">\n' if noindex else ""
    return f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{esc(full)}</title>
<meta name="description" content="{esc(description)}">
{robots}<link rel="canonical" href="{esc(url)}">
<link rel="icon" href="{BASE}favicon.svg" type="image/svg+xml">
<link rel="apple-touch-icon" href="{BASE}apple-touch-icon.png">
<meta name="theme-color" content="#050505">
<meta property="og:type" content="{kind}">
<meta property="og:site_name" content="{SITE_NAME}">
<meta property="og:title" content="{esc(full)}">
<meta property="og:description" content="{esc(description)}">
<meta property="og:url" content="{esc(url)}">
<meta property="og:image" content="{BASE}og.png">
<meta property="og:image:width" content="1200">
<meta property="og:image:height" content="630">
<meta property="og:image:alt" content="Farpane: VNC-compatible remote desktop viewer and server written in Rust">
<meta name="twitter:card" content="summary_large_image">
<meta name="twitter:title" content="{esc(full)}">
<meta name="twitter:description" content="{esc(description)}">
<meta name="twitter:image" content="{BASE}og.png">
<link rel="stylesheet" href="{{root}}style.css">
{ld}</head>
"""


def chrome(body, root, current=""):
    nav = [
        ("Handbook", f"{root}handbook/", "handbook"),
        ("Security", f"{root}handbook/security/", "security"),
        ("GitHub", REPO, ""),
    ]
    links = "".join(
        f'<a href="{href}"{" aria-current=page" if key and key == current else ""}>{label}</a>'
        for label, href, key in nav
    )
    return f"""<body>
<a class="skip" href="#content">Skip to content</a>
<header class="site"><div class="bar">
<a class="brand" href="{root}"><img src="{root}icon-512.png" alt="" width="32" height="32">Farpane</a>
<nav class="top" aria-label="Main">{links}</nav>
</div></header>
{body}
<footer class="site"><div class="bar">
<span>&copy; 2026 Pierre Jochem. Apache-2.0 or MIT.</span>
<span>"VNC" is a trademark of RealVNC Ltd. Farpane is independent and not affiliated with RealVNC.</span>
</div></footer>
</body>
</html>
"""


def page_url(slug):
    return BASE + ("handbook/" if slug == "introduction" else f"handbook/{slug}/")


def build():
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir()
    for f in (ROOT / "site" / "static").iterdir():
        if f.is_dir():
            shutil.copytree(f, OUT / f.name)
        else:
            shutil.copy(f, OUT / f.name)

    pages = []
    for md in sorted((ROOT / "docs" / "handbook").glob("*.md")):
        meta, body = front_matter(md.read_text())
        pages.append(
            {
                "slug": md.stem,
                "title": meta["title"],
                "description": meta["description"],
                "order": int(meta["order"]),
                "body": body,
                "src": md,
            }
        )
    pages.sort(key=lambda p: p["order"])
    slugs = {p["slug"] for p in pages}
    sitemap = [(BASE, today(), "1.0")]

    for i, p in enumerate(pages):
        is_index = p["slug"] == "introduction"
        out_dir = OUT / "handbook" if is_index else OUT / "handbook" / p["slug"]
        out_dir.mkdir(parents=True, exist_ok=True)
        root = "../" if is_index else "../../"
        sib = "" if is_index else "../"  # path from this page to /handbook/
        md = markdown.Markdown(extensions=["tables", "fenced_code", "toc", "attr_list"])
        content = md.convert(p["body"])

        def fix(m):
            name = m.group(1)
            if name not in slugs:
                return m.group(0)
            return 'href="' + (sib if name == "introduction" else f"{sib}{name}/") + '"'

        content = re.sub(r'href="([a-z]+)\.html"', fix, content)
        content = content.replace('src="screenshots/', f'src="{root}screenshots/')
        # Each page's own h1 stays the only h1; give headings anchors via toc ids.
        toc = "".join(
            f'<li><a href="{sib if q["slug"] == "introduction" else sib + q["slug"] + "/"}"'
            f'{" aria-current=page" if q is p else ""}>{esc(q["title"])}</a></li>'
            for q in pages
        )
        prev = pages[i - 1] if i > 0 else None
        nxt = pages[i + 1] if i + 1 < len(pages) else None

        def link(q, label):
            href = sib if q["slug"] == "introduction" else f'{sib}{q["slug"]}/'
            return f'<a href="{href}" rel="{label.lower()}">{label}: {esc(q["title"])}</a>'

        pager = (
            '<nav class="pager" aria-label="Pages">'
            f'<span>{link(prev, "Previous") if prev else ""}</span>'
            f'<span>{link(nxt, "Next") if nxt else ""}</span></nav>'
        )
        url = page_url(p["slug"])
        modified = last_modified(p["src"])
        ld = [
            {
                "@context": "https://schema.org",
                "@type": "TechArticle",
                "headline": p["title"],
                "description": p["description"],
                "url": url,
                "inLanguage": "en",
                "dateModified": modified,
                "image": BASE + "og.png",
                "author": {"@type": "Person", "name": "Pierre Jochem"},
                "isPartOf": {"@type": "WebSite", "name": SITE_NAME, "url": BASE},
            },
            {
                "@context": "https://schema.org",
                "@type": "BreadcrumbList",
                "itemListElement": [
                    {"@type": "ListItem", "position": 1, "name": "Farpane", "item": BASE},
                    {"@type": "ListItem", "position": 2, "name": "Handbook", "item": BASE + "handbook/"},
                ]
                + (
                    []
                    if is_index
                    else [{"@type": "ListItem", "position": 3, "name": p["title"], "item": url}]
                ),
            },
        ]
        crumbs = (
            f'<p class="crumbs"><a href="{root}">Farpane</a> / '
            + (
                "Handbook"
                if is_index
                else f'<a href="{sib}">Handbook</a> / {esc(p["title"])}'
            )
            + "</p>"
        )
        edit = (
            f'<p class="edit"><a href="{REPO}/edit/main/docs/handbook/{p["slug"]}.md">'
            "Improve this page on GitHub</a></p>"
        )
        body = f"""<main id="content"><div class="layout">
<aside class="toc" aria-label="Handbook"><h2>Handbook</h2><ol>{toc}</ol></aside>
<article>{crumbs}
{content}
{pager}
{edit}</article>
</div></main>"""
        doc = head(p["title"], p["description"], url, "article", ld).replace("{root}", root)
        doc += chrome(body, root, "security" if p["slug"] == "security" else "handbook")
        (out_dir / "index.html").write_text(doc)
        sitemap.append((url, modified, "0.8" if is_index else "0.7"))

    # landing page
    ld = [
        {
            "@context": "https://schema.org",
            "@type": "SoftwareApplication",
            "name": SITE_NAME,
            "applicationCategory": "NetworkingApplication",
            "operatingSystem": "Linux, macOS, Windows",
            "description": LANDING_DESC,
            "url": BASE,
            "image": BASE + "og.png",
            "license": "https://spdx.org/licenses/MIT.html",
            "codeRepository": REPO,
            "programmingLanguage": "Rust",
            "offers": {"@type": "Offer", "price": "0", "priceCurrency": "USD"},
            "author": {"@type": "Person", "name": "Pierre Jochem"},
        },
        {
            "@context": "https://schema.org",
            "@type": "WebSite",
            "name": SITE_NAME,
            "url": BASE,
            "description": TAGLINE,
        },
    ]
    features = [
        ("Viewer and server", "Farpane Viewer shows any RFB server; Farpane Server shares a Linux desktop, X11 or Wayland through the desktop portal."),
        ("Encrypted sign-in", "VeNCrypt over TLS and RSA-AES, certificate and key pinning, and its own accounts with Argon2id, lockout and throttling."),
        ("Standard protocol", "RFB 3.3, 3.7 and 3.8 with Raw, CopyRect, RRE, Hextile, zlib, ZRLE, TRLE and Tight, plus Fence, resize and UTF-8 clipboard."),
        ("A dashboard", "Live traffic, connected clients, view-only and disconnect, user management, security settings and an event log."),
        ("Glass, Slate, Light", "One egui interface with three themes, accent colours and interface sizes, on Linux, macOS and Windows."),
        ("Safe by construction", "A sans-IO protocol core with bounded lengths, fuzz targets and byte-exact tests; unsafe code is denied outside one small crate."),
    ]
    cards = "".join(
        f'<div class="card"><h3>{esc(t)}</h3><p>{esc(d)}</p></div>' for t, d in features
    )
    body = f"""<main id="content">
<section class="hero">
<div>
<span class="badge">Early development, not released yet</span>
<h1>A VNC-compatible remote desktop viewer and server in Rust</h1>
<p class="lead">Connect to TigerVNC, QEMU or x11vnc servers, or share your Linux desktop, over encrypted connections with real user accounts. Free and open source.</p>
<div class="cta"><a class="btn primary" href="handbook/">Read the handbook</a><a class="btn" href="{REPO}">View on GitHub</a></div>
</div>
<img src="icon-512.png" width="200" height="200" alt="Farpane icon: three receding panes">
</section>
<section class="section" aria-labelledby="features"><h2 id="features">What it does</h2><div class="grid">{cards}</div></section>
<section class="section" aria-labelledby="look"><h2 id="look">What it looks like</h2>
<div class="shots">
<figure><img src="screenshots/server-overview.png" width="1100" height="760" loading="lazy" alt="Farpane Server dashboard: live traffic chart, connected clients, shared screen and sign-in methods"><figcaption>Server dashboard (Glass theme)</figcaption></figure>
<figure><img src="screenshots/viewer-session.png" width="1100" height="720" loading="lazy" alt="Farpane Viewer in a live session with the Live stats card and a footer showing protocol and security"><figcaption>Viewer in a live session</figcaption></figure>
<figure><img src="screenshots/server-clients.png" width="1100" height="760" loading="lazy" alt="Farpane Server clients page in the dark Slate theme"><figcaption>Clients page (Slate theme)</figcaption></figure>
<figure><img src="screenshots/viewer-home-light.png" width="1100" height="720" loading="lazy" alt="Farpane Viewer home screen in the Light theme with saved connections"><figcaption>Viewer home (Light theme)</figcaption></figure>
</div></section>
<section class="section" aria-labelledby="try"><h2 id="try">Try it</h2>
<p>There is no release yet, so build from source (Rust 1.88 or newer):</p>
<pre><code>git clone {REPO}.git
cd farpane
cargo build --release --workspace
./target/release/farpane-server --listen 127.0.0.1:5900 &amp;
./target/release/farpane-viewer 127.0.0.1:5900</code></pre>
<p>See <a href="handbook/install/">install and build</a>, <a href="handbook/viewer/">using the viewer</a> and <a href="handbook/server/">running the server</a>. How connections are protected is described in the <a href="handbook/security/">security model</a>, and what is verified in <a href="handbook/protocol/">protocol support</a>.</p></section>
<section class="section" aria-labelledby="note"><h2 id="note">An independent project</h2>
<p>"VNC" is a trademark of RealVNC Ltd. Farpane is independent and not affiliated with RealVNC, which is why it says "VNC-compatible". It is dual-licensed under the Apache License 2.0 or the MIT licence.</p></section>
</main>"""
    doc = head(LANDING_TITLE, LANDING_DESC, BASE, "website", ld, bare=True)
    doc = doc.replace("{root}", "")
    doc += chrome(body, "", "")
    (OUT / "index.html").write_text(doc)

    # 404
    nf = head("Page not found", "This page does not exist.", BASE + "404.html", noindex=True).replace("{root}", BASE)
    nf += chrome(
        f'<main id="content"><h1>Page not found</h1><p>Try the <a href="{BASE}handbook/">handbook</a> or the <a href="{BASE}">start page</a>.</p></main>',
        BASE,
    )
    (OUT / "404.html").write_text(nf)

    urls = "".join(
        f"<url><loc>{esc(u)}</loc><lastmod>{d}</lastmod><priority>{pr}</priority></url>\n"
        for u, d, pr in sitemap
    )
    (OUT / "sitemap.xml").write_text(
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n' + urls + "</urlset>\n"
    )
    (OUT / "robots.txt").write_text(f"User-agent: *\nAllow: /\n\nSitemap: {BASE}sitemap.xml\n")
    (OUT / ".nojekyll").write_text("")
    print(f"built {len(sitemap)} pages into {OUT}")


if __name__ == "__main__":
    build()
