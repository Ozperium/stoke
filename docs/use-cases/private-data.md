---
title: Keep client documents off the cloud
description: Summarize a client’s unreleased plans on approved private infrastructure, without a cloud fallback quietly changing where the document goes.
slug: private-data
image: /media/use-cases/private-data-v1.webp
og_image: /media/use-cases/private-data-og-v2.png
image_alt: Private documents kept inside a boundary, separate from a cloud destination.
persona: Teams handling confidential data
icon: shield
order: 2
---
# Keep client documents off the cloud

A client sends you an unreleased product plan. You need a short briefing before the next meeting. Pasting it into your usual cloud assistant would be convenient, but the client has not approved sending it there.

The difficult part is not choosing a private model. It is making sure a busy or unavailable private model does not turn into an accidental cloud request.

## Make the destination part of the rule

Connect your compatible application to a Stoke route restricted to approved private destinations. That restriction applies before dispatch and during fallback. If no permitted destination can serve the request, it is refused.

An unavailable summary is inconvenient. Sending the document somewhere you did not approve is a different problem. Decide which outcome you want before the first request, not after checking a provider dashboard.

## Removing an email address does not make a plan public

For work already approved for a cloud model, pattern-based redaction can remove recognized secrets from supported message content. It is not complete DLP or anonymization. The surrounding product plan can still be confidential even after every email address is gone.

Use a private route when the material itself must stay private. Use redaction as an additional safeguard for permitted traffic, not as permission to send it anywhere.

## Test the route when the private model is unavailable

Use synthetic documents, take the private destination offline and confirm that the request is refused without any cloud dispatch. Also inspect the actual hosts behind your configured destination tiers; a label does not establish trust.

Review the application's own telemetry, logs and tools separately. Stoke controls the model requests that pass through it, not every way a document might leave your machine.

[Set up an authenticated private model endpoint](/guides/expose-ollama-to-your-network-safely/)
