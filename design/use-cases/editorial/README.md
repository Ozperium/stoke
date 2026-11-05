# Explanatory article illustrations

Six explanatory illustrations in the approved editorial direction: owned-machine routing, private routes, metered spending limits, gateway/provider credentials, connection recovery and recognized-secret redaction. SVG files are the editable source of truth. The article raster is exported at 1152×720 with a transparent background, using the site Sora font, then converted with `cwebp -q 90`. Page-specific social cards are 1200×630 PNGs. Hashes and paths are in manifest.json.

The redaction image uses an all-zero dummy value matching the built-in API-key pattern in src/builtins.rs; its output marker matches that implementation. It is an explanation, not a product-interface screenshot or a new inference test. The complete six-page set is a preview; approval of the illustration direction does not itself publish it to production.
