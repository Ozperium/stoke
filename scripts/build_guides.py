#!/usr/bin/env python3
"""Render docs/guides/*.md into landing/guides/*.html.

Stdlib only, no build step on the host: the generated HTML is committed and
Cloudflare Pages serves it as static files. Run after editing any guide:

    python3 scripts/build_guides.py

Each guide needs front-matter:

    ---
    title: ...
    description: ...
    slug: ...
    ---
"""
import html
import os
import re
import sys
from datetime import date

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "docs", "guides")
OUT = os.path.join(ROOT, "landing", "guides")
SITE = "https://stokegate.com"

# ── Minimal markdown → HTML ───────────────────────────────────────────────
# Deliberately small: we control the input, so we support exactly what the
# guides use (fenced code, headings, lists, tables, links, inline code, bold).


def esc(s):
    return html.escape(s, quote=False)


def inline(s):
    s = esc(s)
    s = re.sub(r"`([^`]+)`", r"<code>\1</code>", s)
    s = re.sub(r"\*\*([^*]+)\*\*", r"<strong>\1</strong>", s)
    s = re.sub(r"(?<!\*)\*([^*\n]+)\*(?!\*)", r"<em>\1</em>", s)
    s = re.sub(r"\[([^\]]+)\]\(([^)]+)\)", r'<a href="\2">\1</a>', s)
    return s



# ── Build-time syntax highlighting (stdlib, no client JS) ────────────────
# Emits static <span> tokens so pages render highlighted with JavaScript
# disabled — good for crawlers and for a site that makes zero external
# requests. Each language's rules put COMMENTS and STRINGS first, so a "#"
# or a keyword inside a quoted string is never mis-tokenised.

_STR = r'''(?P<str>[rbfuRBFU]{0,2}"""(?:\\.|[^\\])*?"""|[rbfuRBFU]{0,2}\'\'\'(?:\\.|[^\\])*?\'\'\'|[rbfuRBFU]{0,2}"(?:\\.|[^"\\\n])*"|[rbfuRBFU]{0,2}\'(?:\\.|[^\'\\\n])*\')'''

_PY_KW = r"\b(?:def|class|return|import|from|for|in|if|elif|else|while|try|except|with|as|and|or|not|is|None|True|False|lambda|yield|pass|break|continue|raise|global)\b"
_PY_BI = r"\b(?:print|len|str|int|dict|list|set|next|open|range|json|re|self)\b"

RULES = {
    "python": re.compile(
        r"(?P<com>#[^\n]*)|" + _STR + r"|"
        r"(?P<kw>" + _PY_KW + r")|"
        r"(?P<bi>" + _PY_BI + r")|"
        r"(?P<num>\b\d+(?:\.\d+)?\b)"
    ),
    "bash": re.compile(
        r"(?P<com>#[^\n]*)|" + _STR + r"|"
        r"(?P<var>\$\{?\w+\}?|(?m:^\s*)[A-Z_][A-Z0-9_]*(?==))|"
        r"(?P<kw>(?m:^\s*)(?:curl|export|python3|stoke|stoke-cli|cargo|git|ollama|pkill|unset|echo|sh|sudo|systemctl|launchctl|tar|chmod|mkdir|cp)\b)|"
        r"(?P<flag>\s-{1,2}[A-Za-z][\w-]*)|"
        r"(?P<num>\b\d+(?:\.\d+)?\b)"
    ),
    "toml": re.compile(
        r"(?P<com>#[^\n]*)|" + _STR + r"|"
        r"(?P<sec>(?m:^\s*)\[{1,2}[^\]\n]+\]{1,2})|"
        r"(?P<key>(?m:^\s*)[A-Za-z_][\w.-]*(?=\s*=))|"
        r"(?P<kw>\b(?:true|false)\b)|"
        r"(?P<num>\b\d+(?:\.\d+)?\b)"
    ),
    "json": re.compile(
        r"(?P<key>\"(?:\\.|[^\"\\])*\"(?=\s*:))|" + _STR + r"|"
        r"(?P<kw>\b(?:true|false|null)\b)|"
        r"(?P<num>-?\b\d+(?:\.\d+)?\b)"
    ),
}
RULES["sh"] = RULES["bash"]
RULES["py"] = RULES["python"]


def highlight(code, lang):
    """Tokenise `code`, escaping every fragment. Unknown languages pass through."""
    rules = RULES.get((lang or "").lower())
    if not rules:
        return esc(code)
    out, pos = [], 0
    for m in rules.finditer(code):
        out.append(esc(code[pos:m.start()]))
        out.append(f'<span class="tok-{m.lastgroup}">{esc(m.group())}</span>')
        pos = m.end()
    out.append(esc(code[pos:]))
    return "".join(out)


def render(md):
    lines = md.split("\n")
    out, i = [], 0
    while i < len(lines):
        ln = lines[i]

        if ln.startswith("```"):
            lang = ln[3:].strip()
            i += 1
            buf = []
            while i < len(lines) and not lines[i].startswith("```"):
                buf.append(lines[i])
                i += 1
            i += 1
            cls = f' class="lang-{esc(lang)}"' if lang else ""
            out.append(f"<pre><code{cls}>{highlight(chr(10).join(buf), lang)}</code></pre>")
            continue

        m = re.match(r"^(#{1,4})\s+(.*)$", ln)
        if m:
            lvl = len(m.group(1))
            text = m.group(2).strip()
            slug = re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")
            if lvl == 1:
                out.append(f"<h1>{inline(text)}</h1>")
            else:
                out.append(f'<h{lvl} id="{slug}">{inline(text)}</h{lvl}>')
            i += 1
            continue

        if ln.strip().startswith("|") and i + 1 < len(lines) and set(lines[i + 1].replace("|", "").strip()) <= set("-: "):
            header = [c.strip() for c in ln.strip().strip("|").split("|")]
            i += 2
            rows = []
            while i < len(lines) and lines[i].strip().startswith("|"):
                rows.append([c.strip() for c in lines[i].strip().strip("|").split("|")])
                i += 1
            th = "".join(f"<th>{inline(c)}</th>" for c in header)
            trs = "".join("<tr>" + "".join(f"<td>{inline(c)}</td>" for c in r) + "</tr>" for r in rows)
            out.append(f"<table><thead><tr>{th}</tr></thead><tbody>{trs}</tbody></table>")
            continue

        if re.match(r"^\s*[-*]\s+", ln):
            items = []
            while i < len(lines) and re.match(r"^\s*[-*]\s+", lines[i]):
                items.append(re.sub(r"^\s*[-*]\s+", "", lines[i]))
                i += 1
            out.append("<ul>" + "".join(f"<li>{inline(x)}</li>" for x in items) + "</ul>")
            continue

        if re.match(r"^\s*\d+\.\s+", ln):
            items = []
            while i < len(lines) and re.match(r"^\s*\d+\.\s+", lines[i]):
                items.append(re.sub(r"^\s*\d+\.\s+", "", lines[i]))
                i += 1
            out.append("<ol>" + "".join(f"<li>{inline(x)}</li>" for x in items) + "</ol>")
            continue

        if ln.startswith("> "):
            buf = []
            while i < len(lines) and lines[i].startswith("> "):
                buf.append(lines[i][2:])
                i += 1
            out.append(f"<blockquote>{inline(' '.join(buf))}</blockquote>")
            continue

        if ln.strip() in ("---", "***"):
            out.append("<hr>")
            i += 1
            continue

        if ln.strip() == "":
            i += 1
            continue

        buf = []
        while i < len(lines) and lines[i].strip() and not re.match(r"^(#{1,4}\s|```|\s*[-*]\s|\s*\d+\.\s|>\s|\|)", lines[i]):
            buf.append(lines[i])
            i += 1
        if buf:
            out.append(f"<p>{inline(' '.join(buf))}</p>")

    return "\n".join(out)


def front_matter(md):
    m = re.match(r"^---\n(.*?)\n---\n(.*)$", md, re.S)
    if not m:
        raise SystemExit("missing front-matter")
    meta = {}
    for line in m.group(1).split("\n"):
        if ":" in line:
            k, v = line.split(":", 1)
            meta[k.strip()] = v.strip()
    return meta, m.group(2)



# ── Inline SVG art (no external requests; the whole site is self-contained) ──
_SVG = 'viewBox="0 0 48 48" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"'
ICONS = {
    # shield + keyhole: keep the model server private
    "shield": f'<svg {_SVG}><path d="M24 5 39 11v12c0 9-6.5 16.5-15 20-8.5-3.5-15-11-15-20V11Z"/><rect x="19" y="22" width="10" height="9" rx="1.5"/><path d="M21.5 22v-3a2.5 2.5 0 0 1 5 0v3"/></svg>',
    # funnel + block: refuse the prompt before the model
    "filter": f'<svg {_SVG}><path d="M7 9h34L28 24v13l-8 4V24Z"/><circle cx="36" cy="34" r="7.5"/><path d="M31 39l10-10"/></svg>',
    # gauge: a hard ceiling on spend
    "gauge": f'<svg {_SVG}><path d="M9 34a15 15 0 1 1 30 0"/><path d="M24 34l9-12"/><circle cx="24" cy="34" r="2.4" fill="currentColor" stroke="none"/><path d="M8 34h4M36 34h4M24 17v4"/></svg>',
    # spend climbing, then flatlining at the breaker
    "flatline": f'<svg {_SVG}><path d="M5 34l6-9 5 11 5-16 4 14"/><path d="M25 34h18"/><path d="M25 14v27" stroke-dasharray="3 3" opacity=".55"/><circle cx="25" cy="34" r="2.6" fill="currentColor" stroke="none"/></svg>',
    # two machines, one link
    "nodes": f'<svg {_SVG}><rect x="4" y="13" width="15" height="12" rx="2.5"/><rect x="29" y="13" width="15" height="12" rx="2.5"/><path d="M19 19h4M25 19h4"/><circle cx="24" cy="19" r="2.3" fill="currentColor" stroke="none"/><path d="M11.5 25v6M7 31h9M36.5 25v6M32 31h9"/></svg>',
}
CHIP_COLOR = {"Security": "var(--accent)", "Cost": "#e8b339", "Routing": "#7dd3fc"}

# ── Page shell (matches the landing's design tokens) ──────────────────────
CSS = """
:root{--bg:#0a0a0b;--bg2:#131316;--bg3:#1a1a20;--border:#2a2a32;--text:#e4e4e7;--muted:#8a8a96;--accent:#3bdb78;--red:#ff5454;
--font:-apple-system,BlinkMacSystemFont,'Segoe UI',system-ui,sans-serif;--mono:ui-monospace,SFMono-Regular,'SF Mono',Menlo,monospace}
*{box-sizing:border-box;margin:0;padding:0}
body{background:var(--bg);color:var(--text);font-family:var(--font);line-height:1.7;-webkit-font-smoothing:antialiased}
a{color:var(--accent);text-decoration:none}a:hover{text-decoration:underline}
nav{border-bottom:1px solid var(--border);position:sticky;top:0;background:rgba(10,10,11,.85);backdrop-filter:blur(8px);z-index:10}
.nav-inner{max-width:860px;margin:0 auto;padding:16px 24px;display:flex;justify-content:space-between;align-items:center}
.logo{font-weight:700;font-size:17px;color:var(--text)}
.nav-links{display:flex;gap:20px}.nav-links a{color:var(--muted);font-size:14px}
main{max-width:760px;margin:0 auto;padding:56px 24px 80px}
h1{font-size:40px;line-height:1.15;letter-spacing:-.02em;margin-bottom:16px}
h2{font-size:24px;margin:44px 0 14px;letter-spacing:-.01em;scroll-margin-top:80px}
h3{font-size:18px;margin:28px 0 10px}
p{margin:14px 0;color:#c9c9d1}
ul,ol{margin:14px 0 14px 22px}li{margin:7px 0;color:#c9c9d1}
code{font-family:var(--mono);font-size:.88em;color:var(--accent);background:var(--bg3);padding:2px 6px;border-radius:5px}
pre{background:var(--bg2);border:1px solid var(--border);border-radius:10px;padding:18px;overflow-x:auto;margin:18px 0}
pre code{background:none;color:#d9d9e3;padding:0;font-size:13.5px;line-height:1.65}
blockquote{border-left:3px solid var(--accent);background:var(--bg2);padding:12px 18px;margin:18px 0;border-radius:0 8px 8px 0;color:var(--muted)}
table{width:100%;border-collapse:collapse;margin:18px 0;font-size:14.5px}
th,td{border:1px solid var(--border);padding:9px 12px;text-align:left}
th{background:var(--bg2);font-weight:600}
hr{border:0;border-top:1px solid var(--border);margin:36px 0}
.lede{color:var(--muted);font-size:18px;margin-bottom:8px}
.crumb{font-family:var(--mono);font-size:13px;color:var(--muted);margin-bottom:22px}
.cta{margin-top:52px;border:1px solid var(--border);background:var(--bg2);border-radius:14px;padding:26px}
.cta h3{margin-top:0}
.btn{display:inline-block;background:var(--accent);color:#07120b;font-weight:650;padding:11px 20px;border-radius:9px;margin-top:10px}
.btn:hover{text-decoration:none;opacity:.92}
footer{border-top:1px solid var(--border);padding:28px 24px;color:var(--muted);font-size:13.5px;text-align:center}
@media(max-width:640px){h1{font-size:31px}main{padding:36px 20px 60px}.nav-links{display:none}}

/* syntax tokens (build-time highlighted; no client JS) */
.tok-com{color:var(--muted);font-style:italic}
.tok-str{color:#7dd3fc}
.tok-num{color:#e8b339}
.tok-kw{color:#3bdb78;font-weight:600}
.tok-bi{color:#6ee7a8}
.tok-var{color:#e8b339}
.tok-key{color:#e4e4e7}
.tok-sec{color:#3bdb78;font-weight:600}
.tok-flag{color:#8a8a96}

/* guide index cards */
.gwrap{display:grid;grid-template-columns:1fr 1fr;gap:20px;margin-top:36px}
.gcard{display:block;border:1px solid var(--border);background:var(--bg2);border-radius:16px;overflow:hidden;transition:border-color .18s,transform .18s}
.gcard:hover{border-color:rgba(59,219,120,.45);transform:translateY(-2px);text-decoration:none}
.gart{height:118px;display:flex;align-items:center;justify-content:center;background:radial-gradient(circle at 50% 120%,rgba(59,219,120,.14),transparent 68%);border-bottom:1px solid var(--border)}
.gart svg{width:52px;height:52px;color:var(--accent);opacity:.92}
.gbody{padding:20px 22px 22px}
.gchip{display:inline-block;font-family:var(--mono);font-size:10.5px;letter-spacing:.05em;text-transform:uppercase;border:1px solid var(--border);border-radius:99px;padding:3px 9px;margin-bottom:11px}
.gcard h2{font-size:18px;margin:0 0 7px;color:var(--text);letter-spacing:-.01em;line-height:1.35}
.gcard p{font-size:14px;color:var(--muted);margin:0 0 14px;line-height:1.6}
.gcard .go{font-size:13.5px;color:var(--accent);font-weight:600}
@media(max-width:700px){.gwrap{grid-template-columns:1fr}}

"""

PAGE = """<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<link rel="icon" href="/favicon.svg" type="image/svg+xml">
<link rel="mask-icon" href="/favicon.svg" color="#3bdb78">
<meta name="theme-color" content="#0a0a0b">
<title>{title} — Stoke</title>
<meta name="description" content="{description}">
<link rel="canonical" href="{canonical}">
<meta property="og:type" content="article">
<meta property="og:title" content="{title}">
<meta property="og:description" content="{description}">
<meta property="og:url" content="{canonical}">
<script type="application/ld+json">{jsonld}</script>
<style>{css}</style>
</head>
<body>
<nav><div class="nav-inner">
  <a href="/" class="logo">⬡ Stoke</a>
  <div class="nav-links">
    <a href="/guides/">Guides</a>
    <a href="https://github.com/Ozperium/stoke">GitHub</a>
  </div>
</div></nav>
<main>
<div class="crumb"><a href="/">Stoke</a> / <a href="/guides/">Guides</a></div>
{body}
<div class="cta">
  <h3>Put a control point in front of your agents</h3>
  <p>Stoke is a single Rust binary: hard budget caps, a runaway-loop kill switch, and local-first routing — enforced before a provider is ever called. Open source, MIT.</p>
  <a class="btn" href="https://github.com/Ozperium/stoke">Get Stoke on GitHub</a>
</div>
</main>
<footer>MIT licensed · <a href="https://stokegate.com">stokegate.com</a> · <a href="https://github.com/Ozperium/stoke">GitHub</a></footer>
</body>
</html>
"""

def card(m):
    icon = ICONS.get(m.get("icon", "shield"), ICONS["shield"])
    cat = m.get("category", "Guide")
    color = CHIP_COLOR.get(cat, "var(--muted)")
    return (
        f'<a class="gcard" href="/guides/{m["slug"]}/">'
        f'<div class="gart">{icon}</div>'
        f'<div class="gbody">'
        f'<span class="gchip" style="color:{color};border-color:{color}44">{esc(cat)}</span>'
        f'<h2>{esc(m["title"])}</h2><p>{esc(m["description"])}</p>'
        f'<span class="go">Read the guide →</span>'
        f"</div></a>"
    )


def jsonld(meta, url):
    import json
    return json.dumps({
        "@context": "https://schema.org",
        "@type": "TechArticle",
        "headline": meta["title"],
        "description": meta["description"],
        "url": url,
        "author": {"@type": "Organization", "name": "Stoke"},
        "publisher": {"@type": "Organization", "name": "Stoke"},
        "dateModified": date.today().isoformat(),
    }, separators=(",", ":"))


def main():
    if not os.path.isdir(SRC):
        raise SystemExit(f"no guides at {SRC}")
    os.makedirs(OUT, exist_ok=True)
    built = []

    for fn in sorted(os.listdir(SRC)):
        if not fn.endswith(".md"):
            continue
        meta, body_md = front_matter(open(os.path.join(SRC, fn)).read())
        for k in ("title", "description", "slug"):
            if k not in meta:
                raise SystemExit(f"{fn}: front-matter missing '{k}'")
        url = f"{SITE}/guides/{meta['slug']}/"
        page = PAGE.format(
            title=esc(meta["title"]),
            description=esc(meta["description"]),
            canonical=url,
            jsonld=jsonld(meta, url),
            css=CSS,
            body=render(body_md),
        )
        # Directory-style: /guides/<slug>/index.html resolves as /guides/<slug>/
        # on Cloudflare Pages, python http.server, nginx — everywhere.
        destdir = os.path.join(OUT, meta["slug"])
        os.makedirs(destdir, exist_ok=True)
        open(os.path.join(destdir, "index.html"), "w").write(page)
        built.append(meta)
        print(f"  {fn} -> landing/guides/{meta['slug']}/index.html")

    order = {"Security": 0, "Cost": 1, "Routing": 2}
    built.sort(key=lambda m: (order.get(m.get("category", ""), 9), m["title"]))
    items = "\n".join(card(m) for m in built)
    index_body = (
        "<h1>Guides</h1>"
        '<p class="lede">Task-shaped walkthroughs for running agents behind Stoke — '
        "capping spend, killing runaway loops, and keeping prompts on your own hardware. "
        "Every command here is one we run ourselves.</p>"
        f'<div class="gwrap">{items}</div>'
    )
    open(os.path.join(OUT, "index.html"), "w").write(PAGE.format(
        title="Guides",
        description="Task-shaped guides for capping AI agent spend, killing runaway loops, and routing agents to your own machines with Stoke.",
        canonical=f"{SITE}/guides/",
        jsonld=jsonld({"title": "Guides", "description": "Stoke guides"}, f"{SITE}/guides/"),
        css=CSS,
        body=index_body,
    ))
    print(f"  index -> landing/guides/index.html ({len(built)} guides)")

    urls = [f"{SITE}/", f"{SITE}/guides/"] + [f"{SITE}/guides/{m['slug']}/" for m in built]
    today = date.today().isoformat()
    sm = ['<?xml version="1.0" encoding="UTF-8"?>', '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">']
    for u in urls:
        sm.append(f"<url><loc>{u}</loc><lastmod>{today}</lastmod></url>")
    sm.append("</urlset>")
    open(os.path.join(ROOT, "landing", "sitemap.xml"), "w").write("\n".join(sm) + "\n")
    print(f"  sitemap -> landing/sitemap.xml ({len(urls)} urls)")

    open(os.path.join(ROOT, "landing", "robots.txt"), "w").write(
        f"User-agent: *\nAllow: /\n\nSitemap: {SITE}/sitemap.xml\n"
    )
    print("  robots  -> landing/robots.txt")
    return 0


if __name__ == "__main__":
    sys.exit(main())
