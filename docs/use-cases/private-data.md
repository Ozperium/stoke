---
title: Keep private requests private
description: Give confidential AI requests an approved private route. Refuse cloud fallback when policy forbids it, with optional pattern-based redaction for permitted routes.
slug: private-data
persona: Teams handling confidential data
icon: shield
order: 2
---
# Keep private requests private

## A day-to-day case

Your team wants to summarize client documents or troubleshoot internal code. Some requests may use an approved cloud model. Others must stay on infrastructure you control, even when a private model is unavailable.

You need a deliberate boundary, not a promise that the normal route will usually stay private.

## With Stoke

Point the existing compatible application at a configured private route. Restrict its eligible provider tiers to your approved private destinations. Stoke checks this policy before dispatch, including fallback. If no permitted destination can serve the request, it is refused instead of being sent to a cloud model.

For a separately approved cloud route, opt-in, pattern-based redaction can remove recognized secrets and email patterns from supported message content before forwarding. That is a different decision: redact and continue, rather than deny a destination.

## Know the boundary

Destination tiers are configuration, not a certificate of trust. Verify the actual hosts behind them. Redaction is not complete DLP, anonymization or a guarantee that confidential information cannot leave. Test the exact protocol and fields you intend to use; do not assume attachment, tool-argument or response coverage.

Stoke controls model traffic that passes through it. Application telemetry, logs, caches and agent tools need their own review. This setup is not a blanket compliance certification.

## Try this setup

Configure an approved private route, then make its model unavailable. Confirm that the request is refused and no cloud provider receives it. Test redaction separately with synthetic data by inspecting what the upstream destination actually receives.

[Review the routing and redaction capability boundaries](/gateway/)

[Connect an inference server without exposing it unprotected](/guides/expose-ollama-to-your-network-safely/)
