# Stoke presentation contract

The source-of-truth token specification is ../DESIGN.md; the implemented theme is landing/presentation.css.

## Routes
- `/`: short promise, original routing illustration, user-initiated product film, two clear next actions. No form.
- `/cloud/`: short Cloud benefit and request-access handoff. No form or repeated readiness caveats.
- `/join/`: dedicated access request. Email, solo/team choice, optional use case, purpose/retention notice and required consent.
- Gateway/Guides: technical detail and installation, not homepage copy.

## Control budget
- Action: primary / secondary.
- Navigation: plain link / compact secondary action.
- Link roles: inverse skip link / brand wordmark / regular navigation or inline link / emphasized path link. Regular links share one 16px type treatment across navigation, film, footer and consent.
- Film: plain trigger link / close icon.
- Inputs: email / multiline optional text; native radio and checkbox.

All controls retain hover, visible keyboard focus and pressed behavior. Inactive/loading states are not new style variants. The signup page must preserve entered data on failure, avoid double submits, and show received status only after202. Native no-JS fallback goes to the real server form. No fake account, email-sent or activation state.

## Motion and accessibility

No ambient animation is necessary to understand the page. The film is an illustrated overview, with no simulated live UI or native-client compatibility claim. No autoplay. Use native dialog/video behavior with Escape, pause-on-close, focus return and a normal video link when scripting is absent. Reduced motion keeps the static visual art direction. The hidden attribute must remain hidden even when component display rules apply.
