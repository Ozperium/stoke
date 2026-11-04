# Stoke — site-wide design system

## Scope and route map
All public HTML routes use the approved homepage identity: true off-white (#FCFCFC), ink (#191919), coral (#F15332), self-hosted Sora display and Source Sans 3 body. Keep homepage/Cloud scenes and dedicated signup behavior; extend the identity, not the hero animation, to reading surfaces. No backend or native behavior changes.

- `/cloud/` — marketing; shared brand/nav/footer/type/focus system, content retained.
- `/contact/` — document; shared brand/nav/footer/type/focus system, content retained.
- `/gateway/` — document; shared brand/nav/footer/type/focus system, content retained.
- `/guides/block-prompt-injection-at-the-gateway/` — guide; shared brand/nav/footer/type/focus system, content retained.
- `/guides/cache-identity/` — guide; shared brand/nav/footer/type/focus system, content retained.
- `/guides/cache-policy/` — guide; shared brand/nav/footer/type/focus system, content retained.
- `/guides/cap-claude-code-api-spend/` — guide; shared brand/nav/footer/type/focus system, content retained.
- `/guides/expose-ollama-to-your-network-safely/` — guide; shared brand/nav/footer/type/focus system, content retained.
- `/guides/` — guide; shared brand/nav/footer/type/focus system, content retained.
- `/guides/optional-headroom/` — guide; shared brand/nav/footer/type/focus system, content retained.
- `/guides/run-ollama-across-two-machines/` — guide; shared brand/nav/footer/type/focus system, content retained.
- `/guides/stop-a-runaway-ai-agent/` — guide; shared brand/nav/footer/type/focus system, content retained.
- `/guides/use-codex-and-ollama-with-stoke/` — guide; shared brand/nav/footer/type/focus system, content retained.
- `/` — marketing; shared brand/nav/footer/type/focus system, content retained.
- `/join/` — signup; shared brand/nav/footer/type/focus system, content retained.
- `/legal/` — document; shared brand/nav/footer/type/focus system, content retained.
- `/security/` — document; shared brand/nav/footer/type/focus system, content retained.

- `/404.html` — error recovery; same shared shell and readable Home/Guides actions, real missing-route404 behavior retained.

## Shared shell
Use landing/presentation.css and presentation.js on every route. Header: brand bullet+stoke wordmark, responsive Menu with Escape/focus restoration, Gateway / Cloud / Guides / GitHub / outlined Try Cloud→/join/. Footer: same wordmark and Gateway / Cloud / Guides / Security / Legal / Contact / GitHub links, copyright. Add aria-current for the relevant nav item. No old hexagon brand, dark/green document theme, serif/italic headlines, or separate navigation implementation.

## Layout families
- Marketing: preserve approved product-visual composition and sparse copy.
- Signup: preserve fields, consent/notice, no-JS fallback, state behavior and privacy.
- Documents: `<body class="document-page">`, `<main id="main" class="document-shell">`; .document-hero for breadcrumb/h1/lede and .document-content for reading content. Wide capability table uses .document-wide/.matrix-wrap; compact reading text remains ~65–75ch. Shared new `/document.css` extends presentation.css without overriding marketing/form styles.
- Guide index: readable catalogue using existing meaningful guide data/icons; no neon cards/gradients.
- Guide article: complete Markdown body, anchors, syntax-highlighted commands and tables retained. Renderer remains stdlib and build-generated output reproducible.

## Component inventory and states
- Buttons: existing primary, secondary, compact nav CTA, visual playback and scenario controls (5 intentional roles maximum). No new document-specific button system; use .button.button-primary or .button.button-secondary. Default/hover/focus-visible/pressed; disabled/loading only where behavior needs them.
- Links: brand, skip, navigation (header/footer share one size), breadcrumb, prose, catalogue row and compact signup disclosure (7 intentional painted roles). Current navigation via aria-current, not extra palette. The compact disclosure belongs only to the signup task; it is not another reading-link style.
- Inputs: one existing signup text-field role; two native toggle roles (radio and consent checkbox), unchanged. Retain pending/unavailable/error/received states. Playback and consent are legitimate task-specific controls, not new document variants.
- Reading heading: Sora 620, max48px; homepage display remains max72px. Body17px, document line-height1.7; metadata14px; code14px monospace. Legible dark syntax tokens on neutral code panels.
- Surfaces: page off-white, neutral code/table surfaces, restrained coral tint for a callout; one full subtle border, rounded panels. No thick colored side stripes, glass or arbitrary feature grids.
- Tables remain semantic tables in horizontally scrollable wrappers, not cards; no document-wide horizontal overflow.

## Acceptance gates
Inventory18 HTML surfaces (17 subpages including the error document plus homepage reference). Inspect/check every route at390/768/1440; verify computed shared shell/style, menu and keyboard focus, no-JS navigation/reading, reduced motion, exact code blocks/anchors, 24 capability rows, SEO/canonicals/noindex and complete sitemap semantics. Preserve installer and request-form contracts. Confirm generation does not revert design. Review final screenshots and system control variants; keep an exact per-route receipt. Run existing regressions plus focused tests, publish an exact-commit preview, then merge and verify live route assets/content.

## Moment of care and motion budget
Readable commands/tables on a phone, with the same trustworthy shell as the homepage. No new animation families on document pages; existing150ms button interaction only. No extra art peak.
