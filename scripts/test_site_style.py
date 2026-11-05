#!/usr/bin/env python3
"""Cross-route presentation contract; stdlib, no browser dependency."""
from html.parser import HTMLParser
from pathlib import Path
import runpy
import re
from urllib.parse import urlparse

ROOT = Path(__file__).resolve().parents[1]
SITE = ROOT / 'landing'
SPECIAL = {'/', '/cloud/', '/join/'}
BRAND_MARKUP = '<span class="brand-mark" aria-hidden="true"><img src="/stoke-mark.svg" alt="" width="24" height="24"></span>stoke'
ACCEPTED_MARK_PATHS = [
    'M5.229 12.164 L8.454 12.213 L13.335 13.404 L17.875 13.436 L22.276 14.458 L26.774 14.788 L27.300 16.073 L26.571 19.101 L25.371 19.741 L20.758 18.789 L16.218 18.757 L10.319 17.625 L5.014 17.284 L4.614 15.107 Z',
    'M15.108 5.364 L15.930 8.789 L18.024 13.667 L19.717 18.904 L21.727 25.302 L20.360 26.225 L17.054 26.790 L16.036 23.117 L13.664 17.914 L12.539 13.216 L10.171 7.586 L11.366 6.193 Z',
    'M24.661 9.669 L22.683 12.463 L18.914 16.201 L16.534 19.442 L12.297 23.700 L10.379 24.933 L8.424 22.770 L7.986 21.164 L11.354 17.872 L14.670 13.591 L18.454 10.136 L20.886 6.539 L22.766 7.290 Z',
]

class Page(HTMLParser):
    def __init__(self):
        super().__init__()
        self.styles, self.scripts, self.primary, self.footer, self.codes = [], [], [], [], []
        self.body = {}; self.h1 = 0; self.nav = None; self.in_code = False; self.chunks = []
        self.ids = set(); self.meta = {}; self.inline_style = False
    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        if 'style' in a or tag == 'style': self.inline_style = True
        if a.get('id'):
            assert a['id'] not in self.ids, 'duplicate id {}'.format(a['id'])
            self.ids.add(a['id'])
        if tag == 'body': self.body = a
        if tag == 'h1': self.h1 += 1
        if tag == 'link' and a.get('rel') == 'stylesheet': self.styles.append(a['href'])
        if tag == 'script' and a.get('src'): self.scripts.append(a['src'])
        if tag == 'meta' and a.get('name'): self.meta[a['name']] = a.get('content', '')
        if tag == 'nav': self.nav = a.get('aria-label')
        if tag == 'a' and self.nav == 'Primary navigation': self.primary.append(a.get('href'))
        if tag == 'a' and self.nav == 'Footer navigation': self.footer.append(a.get('href'))
        if tag == 'code': self.in_code = True; self.chunks = []
    def handle_endtag(self, tag):
        if tag == 'nav': self.nav = None
        if tag == 'code' and self.in_code:
            self.codes.append(''.join(self.chunks)); self.in_code = False
    def handle_data(self, data):
        if self.in_code: self.chunks.append(data)

def parsed(text):
    p = Page(); p.feed(text); return p

def check():
    files = sorted(SITE.rglob('*.html'))
    routes = {'/' if p == SITE/'index.html' else '/' + p.relative_to(SITE).as_posix().removesuffix('index.html'): p for p in files}
    assert len(routes) == 25 and '/404.html' in routes, 'include all public HTML, not only directory indexes'
    primary = ['/gateway/', '/cloud/', '/use-cases/local-models/', '/use-cases/private-data/', '/use-cases/agent-spend/', '/use-cases/private-model-access/', '/use-cases/node-failover/', '/use-cases/secret-redaction/', '/use-cases/', '/guides/', 'https://github.com/Ozperium/stoke', '/join/']
    footer = ['/gateway/', '/cloud/', '/guides/', '/use-cases/', '/security/', '/legal/', '/contact/', 'https://github.com/Ozperium/stoke']
    for route, path in routes.items():
        text = path.read_text(); p = parsed(text)
        assert p.h1 == 1, route + ': one page heading required'
        style_paths = {urlparse(ref).path for ref in p.styles}
        script_paths = {urlparse(ref).path for ref in p.scripts}
        assert '/presentation.css' in style_paths and '/presentation.js' in script_paths, route + ': shared shell missing'
        assert not (style_paths | script_paths) & {'/site.css','/route.css','/site.js'}, route + ': legacy theme linked'
        assert p.primary == primary, route + ': primary navigation drift'
        assert p.footer == footer, route + ': footer navigation drift'
        assert not p.inline_style, route + ': inline styles prevent the external-style contract'
        assert 'data-nav-toggle' in text and 'aria-controls="nav-menu"' in text
        assert 'class="skip-link"' in text and 'id="main"' in text
        assert text.count(BRAND_MARKUP) == 2, route + ': shared image wordmark missing or duplicated'
        assert 'brand-period' not in text and 'class="brand-mark" aria-hidden="true">•</span>' not in text, route + ': trailing-dot or legacy mark remains'
        if route not in SPECIAL:
            assert 'document-page' in (p.body.get('class') or '').split(), route + ': reading surface missing'
            assert '/document.css' in {urlparse(ref).path for ref in p.styles} and 'document-shell' in text
        if route in {'/legal/','/security/','/contact/','/join/','/404.html'}:
            assert p.meta.get('robots') == 'noindex, follow', route + ': noindex changed'
    mark = (SITE / 'stoke-mark.svg').read_text()
    assert re.findall(r'<path d="([^"]+)"', mark) == ACCEPTED_MARK_PATHS, 'approved mark paths changed'
    assert '<circle' not in mark and '<rect' not in mark, 'approved mark acquired backing geometry'
    presentation = (SITE / 'presentation.css').read_text()
    assert 'font-size:1.45rem' in presentation and 'background:transparent' in presentation, 'shared wordmark styling drifted'
    assert 'border-radius:50%' not in presentation, 'shared mark regained circular backing'
    renderer = runpy.run_path(str(ROOT/'scripts/build_guides.py'))
    assert BRAND_MARKUP in renderer['PAGE'] and 'brand-period' not in renderer['PAGE'], 'guide generator brand contract drifted'
    for source in sorted((ROOT/'docs/guides').glob('*.md')):
        meta, body = renderer['front_matter'](source.read_text())
        expected = parsed(renderer['render'](body)).codes
        actual = parsed((SITE/'guides'/meta['slug']/'index.html').read_text()).codes
        assert actual == expected, meta['slug'] + ': literal code changed'
        source_ids = parsed(renderer['render'](body)).ids
        built_ids = parsed((SITE/'guides'/meta['slug']/'index.html').read_text()).ids
        assert source_ids <= built_ids, meta['slug'] + ': heading anchor removed'
    for bad in ('javascript:alert', 'JaVaScRiPt:alert', 'data:text/html,test', 'vbscript:msgbox', '//untrusted.invalid/path'):
        try:
            renderer['inline']('[bad](' + bad + ')')
        except ValueError:
            pass
        else:
            raise AssertionError('unsafe Markdown link accepted: ' + bad)
    safe = renderer['inline']('[safe](https://example.invalid/?a=1&b="two")')
    assert 'href="https://example.invalid/?a=1&amp;b=&quot;two&quot;"' in safe
    class Attributes(HTMLParser):
        def __init__(self):
            super().__init__(); self.keys = []
        def handle_starttag(self, tag, attrs):
            self.keys.extend(k for k, _ in attrs)
    probe = Attributes()
    probe.feed(renderer['render']('```x" onclick="bad\nexample\n```'))
    assert 'onclick' not in probe.keys, 'fence language injected an HTML attribute'
    css = (SITE/'document.css').read_text()
    assert re.search(r'body\.document-page', css) and '#0a0a0b' not in css and '#3bdb78' not in css
    print('Site-style contract passed: {} routes, shared shell, document theme, noindex and all guide code preserved'.format(len(routes)))

if __name__ == '__main__': check()
