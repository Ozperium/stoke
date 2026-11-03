---
version: alpha
name: Stoke Public Launch
description: Premium graphite-and-ivory product storytelling with a restrained ember accent.
colors:
  primary: "#171716"
  secondary: "#F4F0E8"
  tertiary: "#EF774F"
typography:
  display:
    fontFamily: "Arial Narrow, Avenir Next Condensed, Inter, sans-serif"
    fontSize: "6.4rem"
    fontWeight: 800
    lineHeight: 0.91
    letterSpacing: "-0.075em"
  body:
    fontFamily: "Inter, ui-sans-serif, sans-serif"
    fontSize: 1rem
    fontWeight: 400
    lineHeight: 1.55
  mono:
    fontFamily: "SFMono-Regular, Consolas, monospace"
    fontSize: 0.75rem
    fontWeight: 700
    lineHeight: 1.4
rounded:
  sm: 12px
  md: 18px
  lg: 22px
spacing:
  sm: 12px
  md: 24px
  lg: 56px
components:
  button-primary:
    backgroundColor: "{colors.secondary}"
    textColor: "{colors.primary}"
    rounded: "99px"
    padding: "12px 18px"
  button-ember:
    backgroundColor: "{colors.tertiary}"
    textColor: "{colors.primary}"
    rounded: "99px"
    padding: "12px 18px"
---

## Overview

Stoke's public launch uses editorial product storytelling rather than generic SaaS card grids. The dark hero establishes the model-layer promise; ivory sections explain the product and its limits; ember is reserved for actions, state, and pilot status.

## Colors

- **Graphite (#171716):** primary surface for the hero and product sections.
- **Ivory (#F4F0E8):** readable page surface and high-contrast primary action.
- **Ember (#EF774F):** restrained accent for focus, links, pilot labels, and directional emphasis.
- **Sage (#B8CFB5):** secondary status color in clearly labeled preview surfaces.

## Typography

Use the condensed display stack for large statements and the system sans stack for body copy. Monospace is limited to labels, boundary notes, and technical surfaces.

## Layout

Keep wide editorial margins, thin rules, and asymmetric two-column hero sections. On small screens, collapse to one column at 720px and keep every primary action full width below 420px.

## Accessibility

Lead with the real product interface: a large, user-initiated native video and useful UI stills rather than abstract flow boxes. Label all sample-data frames as preview, not a live Cloud session. Preserve native controls and a poster without autoplay. Navigation progressively enhances a visible no-JS menu; focus stays visible and reduced-motion preferences disable nonessential motion.
