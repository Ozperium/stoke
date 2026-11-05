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


def attr(s):
    return html.escape(s, quote=True)


def guide_url(slug):
    if not re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*", slug):
        raise ValueError(f"invalid guide slug: {slug!r}")
    return f"{SITE}/guides/{slug}/"


def safe_href(escaped_url):
    # inline() has already escaped text; decode once, validate, then escape as an attribute.
    url = html.unescape(escaped_url)
    if not url or any(ord(c) <= 32 or ord(c) == 127 or c == chr(92) for c in url):
        raise ValueError("invalid Markdown link destination")
    scheme = re.match(r"^([A-Za-z][A-Za-z0-9+.-]*):", url)
    if url.startswith("//") or (scheme and scheme.group(1).lower() not in {"http", "https", "mailto"}):
        raise ValueError("unsupported Markdown link scheme")
    return attr(url)


def inline(s):
    s = esc(s)
    s = re.sub(r"`([^`]+)`", r"<code>\1</code>", s)
    s = re.sub(r"\*\*([^*]+)\*\*", r"<strong>\1</strong>", s)
    s = re.sub(r"(?<!\*)\*([^*\n]+)\*(?!\*)", r"<em>\1</em>", s)
    s = re.sub(r"\[([^\]]+)\]\(([^)]+)\)", lambda m: '<a href="{}">{}</a>'.format(safe_href(m.group(2)), m.group(1)), s)
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
            cls = f' class="lang-{attr(lang)}"' if lang else ""
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
            th = "".join(f'<th scope="col">{inline(c)}</th>' for c in header)
            trs = "".join("<tr>" + "".join(f"<td>{inline(c)}</td>" for c in r) + "</tr>" for r in rows)
            out.append(f'<div class="table-wrap"><table><thead><tr>{th}</tr></thead><tbody>{trs}</tbody></table></div>')
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


# Persona-led use cases share the guide renderer and presentation shell.
USE_CASES_SRC = os.path.join(ROOT, "docs", "use-cases")
USE_CASES = []
for filename in sorted(os.listdir(USE_CASES_SRC)):
    if filename.endswith(".md"):
        with open(os.path.join(USE_CASES_SRC, filename)) as source:
            meta, markdown = front_matter(source.read())
        for field in ("title", "description", "slug", "persona", "icon", "order"):
            if not meta.get(field):
                raise ValueError(f"{filename}: missing {field}")
        if not re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*", meta["slug"]):
            raise ValueError(f"invalid use-case slug: {meta['slug']!r}")
        USE_CASES.append((meta, markdown))
USE_CASES.sort(key=lambda item: int(item[0]["order"]))
USE_CASES_MENU = (
    '<details class="nav-use-cases" data-use-cases>'
    '<summary>Use cases<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" aria-hidden="true"><path d="m4 6 4 4 4-4"/></svg></summary>'
    '<div class="use-cases-menu">'
    + ''.join(
        f'<a class="use-cases-menu__item" href="/use-cases/{attr(meta["slug"])}/">'
        f'<span class="use-cases-menu__who">{esc(meta["persona"])}</span>'
        f'<strong>{esc(meta["title"])}</strong>'
        '<span class="use-cases-menu__go">Explore use case <span aria-hidden="true">→</span></span></a>'
        for meta, _ in USE_CASES
    )
    + '<a class="use-cases-menu__all" href="/use-cases/">All use cases <span aria-hidden="true">→</span></a></div></details>'
)


# ── Shared presentation shell ─────────────────────────────────────────────
# Guide pages deliberately load the same external shell as the approved landing.
PAGE = """<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="theme-color" content="#FCFCFC">
  <link rel="icon" href="/favicon.svg" type="image/svg+xml">
  <link rel="mask-icon" href="/favicon.svg" color="#F15332">
  <title>{title} — Stoke</title>
  <meta name="description" content="{description}">
  <link rel="canonical" href="{canonical}">
  <meta property="og:type" content="{og_type}">
  <meta property="og:site_name" content="Stoke">
  <meta property="og:title" content="{title}">
  <meta property="og:description" content="{description}">
  <meta property="og:url" content="{canonical}">
  <meta property="og:image" content="{site}/og-image.png">
  <meta property="og:image:width" content="1200">
  <meta property="og:image:height" content="630">
  <meta property="og:image:alt" content="Your models. Wherever you work.">
  <meta name="twitter:card" content="summary_large_image">
  <meta name="twitter:title" content="{title}">
  <meta name="twitter:description" content="{description}">
  <meta name="twitter:image" content="{site}/og-image.png">
  <meta name="twitter:image:alt" content="Your models. Wherever you work.">
  <script type="application/ld+json">{jsonld}</script>
  <link rel="stylesheet" href="/presentation.css">
  <link rel="stylesheet" href="/document.css">
</head>
<body class="document-page">
  <a class="skip-link" href="#main">Skip to content</a>
  <header class="site-nav"><div class="nav-inner"><a class="brand" href="/" aria-label="Stoke home"><span class="brand-mark" aria-hidden="true">•</span>stoke<span class="brand-period" aria-hidden="true">.</span></a><button class="nav-toggle" type="button" data-nav-toggle aria-controls="nav-menu" aria-expanded="false">Menu</button><nav class="nav-links" id="nav-menu" data-nav-menu aria-label="Primary navigation"><a href="/gateway/">Gateway</a><a href="/cloud/">Cloud</a><!-- use-cases --><a href="/guides/" aria-current="page">Guides</a><a href="https://github.com/Ozperium/stoke">GitHub</a><a class="nav-cta button button-secondary" href="/join/">Try Cloud</a></nav></div></header>
  <main id="main" class="document-shell">
    {body}
  </main>
  <footer class="site-footer"><div class="footer-inner"><a class="brand" href="/" aria-label="Stoke home"><span class="brand-mark" aria-hidden="true">•</span>stoke<span class="brand-period" aria-hidden="true">.</span></a><nav aria-label="Footer navigation"><a href="/gateway/">Gateway</a><a href="/cloud/">Cloud</a><a href="/guides/">Guides</a><a href="/use-cases/">Use cases</a><a href="/security/">Security</a><a href="/legal/">Legal</a><a href="/contact/">Contact</a><a href="https://github.com/Ozperium/stoke">GitHub</a></nav><span>© <span data-year>2026</span> Stoke</span></div></footer>
  <script src="/presentation.js" defer></script>
  <script src="/tracking.js" defer></script>
</body>
</html>
""".replace("<!-- use-cases -->", USE_CASES_MENU)

def article_parts(md):
    """Keep the source's first heading in the shared document hero."""
    match = re.match(r"^\s*#\s+([^\n]+)\n?", md)
    if not match:
        return "", md
    return inline(match.group(1).strip()), md[match.end():]


def category_class(category):
    return re.sub(r"[^a-z0-9]+", "-", category.lower()).strip("-") or "guide"


def card(m):
    icon = ICONS.get(m.get("icon", "shield"), ICONS["shield"])
    cat = m.get("category", "Guide")
    cat_class = category_class(cat)
    return (
        f'<a class="gcard" href="/guides/{m["slug"]}/">'
        f'<div class="gart">{icon}</div>'
        f'<div class="gbody">'
        f'<span class="gchip gchip--{cat_class}">{esc(cat)}</span>'
        f'<h2>{esc(m["title"])}</h2><p>{esc(m["description"])}</p>'
        f'<span class="go">Read the guide →</span>'
        f"</div></a>"
    )


def jsonld(meta, url, schema_type="TechArticle"):
    import json
    data = {
        "@context": "https://schema.org",
        "@type": schema_type,
        "description": meta["description"],
        "url": url,
        "author": {"@type": "Organization", "name": "Stoke"},
        "publisher": {"@type": "Organization", "name": "Stoke"},
        "dateModified": date.today().isoformat(),
    }
    data["name" if schema_type == "CollectionPage" else "headline"] = meta["title"]
    return json.dumps(data, separators=(",", ":")).replace("<", "\\u003c")


def document_body(hero, content):
    return (
        '<section class="document-hero">'
        '<div class="crumb"><a href="/">Stoke</a> / <a href="/guides/">Guides</a></div>'
        f"{hero}</section>"
        f'<div class="document-content">{content}'
        '<aside class="doc-callout">'
        '<h2>Put a control point in front of your agents</h2>'
        '<p>Stoke is a single Rust binary: hard budget caps, a runaway-loop kill switch, and local-first routing — enforced before a provider is ever called. Open source, MIT.</p>'
        '<div class="doc-actions"><a class="button button-primary" href="https://github.com/Ozperium/stoke">Get Stoke on GitHub</a></div>'
        '</aside></div>'
    )


def build_use_cases():
    urls = []
    template = PAGE.replace('href="/guides/" aria-current="page"', 'href="/guides/"')
    for meta, markdown in USE_CASES:
        heading, article = article_parts(markdown)
        url = f'{SITE}/use-cases/{meta["slug"]}/'
        hero = (
            '<section class="document-hero">'
            '<div class="crumb"><a href="/">Stoke</a> / <a href="/use-cases/">Use cases</a></div>'
            f'<p class="eyebrow">{esc(meta["persona"])}</p>'
            f'<h1>{heading}</h1><p class="lede">{esc(meta["description"])}</p></section>'
        )
        page = template.format(
            title=attr(meta["title"]), description=attr(meta["description"]),
            canonical=attr(url), site=attr(SITE), og_type="article",
            jsonld=jsonld(meta, url),
            body=hero + '<div class="document-content">' + render(article)
                 + '<div class="doc-actions"><a class="button button-secondary" href="/use-cases/">Explore other use cases</a></div></div>',
        )
        page = page.replace(f'href="/use-cases/{meta["slug"]}/"', f'href="/use-cases/{meta["slug"]}/" aria-current="page"', 1)
        destination = os.path.join(ROOT, "landing", "use-cases", meta["slug"])
        os.makedirs(destination, exist_ok=True)
        with open(os.path.join(destination, "index.html"), "w") as output:
            output.write(page)
        urls.append(url)

    cards = ''.join(
        f'<a class="gcard" href="/use-cases/{attr(meta["slug"])}/">'
        f'<div class="gart" aria-hidden="true">{ICONS[meta["icon"]]}</div><div class="gbody">'
        f'<p class="eyebrow">{esc(meta["persona"])}</p><h2>{esc(meta["title"])}</h2>'
        f'<p>{esc(meta["description"])}</p><span class="go">Explore use case →</span></div></a>'
        for meta, _ in USE_CASES
    )
    meta = {"title": "Use cases", "description": "Find your Stoke use case: connect models across your machines, keep private requests private, or put limits around agent spend."}
    url = f"{SITE}/use-cases/"
    body = (
        '<section class="document-hero"><div class="crumb"><a href="/">Stoke</a> / Use cases</div>'
        '<h1>Your work.<br>Your use case.</h1>'
        '<p class="lede">Start with what you need to do. See where Stoke fits, what it controls, and where its boundaries are.</p></section>'
        f'<div class="document-content"><div class="gwrap">{cards}</div></div>'
    )
    page = template.format(
        title=attr(meta["title"]), description=attr(meta["description"]),
        canonical=attr(url), site=attr(SITE), og_type="website",
        jsonld=jsonld(meta, url, "CollectionPage"), body=body,
    ).replace('class="use-cases-menu__all" href="/use-cases/"', 'class="use-cases-menu__all" href="/use-cases/" aria-current="page"', 1)
    with open(os.path.join(ROOT, "landing", "use-cases", "index.html"), "w") as output:
        output.write(page)
    print(f"  use cases -> {len(USE_CASES)} details and index")
    return [url] + urls


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
        url = guide_url(meta["slug"])
        article_title, article_md = article_parts(body_md)
        hero = (
            f'<h1>{article_title or esc(meta["title"])}</h1>'
            f'<p class="lede">{esc(meta["description"])}</p>'
        )
        page = PAGE.format(
            title=attr(meta["title"]),
            description=attr(meta["description"]),
            canonical=attr(url),
            site=attr(SITE),
            og_type="article",
            jsonld=jsonld(meta, url),
            body=document_body(hero, render(article_md)),
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
    index_body = f'<div class="gwrap">{items}</div>'
    index_hero = (
        "<h1>Guides</h1>"
        '<p class="lede">Task-shaped walkthroughs for running agents behind Stoke — '
        "capping spend, killing runaway loops, and keeping prompts on your own hardware. "
        "Every command here is one we run ourselves.</p>"
    )
    open(os.path.join(OUT, "index.html"), "w").write(PAGE.format(
        title="Guides",
        description="Task-shaped guides for capping AI agent spend, killing runaway loops, and routing agents to your own machines with Stoke.",
        canonical=attr(f"{SITE}/guides/"),
        site=attr(SITE),
        og_type="website",
        jsonld=jsonld({"title": "Guides", "description": "Stoke guides"}, f"{SITE}/guides/", "CollectionPage"),
        body=document_body(index_hero, index_body),
    ))
    print(f"  index -> landing/guides/index.html ({len(built)} guides)")

    urls = [f"{SITE}/", f"{SITE}/guides/"] + [f"{SITE}/guides/{m['slug']}/" for m in built]
    urls += build_use_cases()
    urls += [f"{SITE}/{route}/" for route in ("cloud", "gateway", "security", "legal", "contact")
             if os.path.isfile(os.path.join(ROOT, "landing", route, "index.html"))]
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
