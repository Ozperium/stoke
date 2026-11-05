---
title: Recover when a model node goes offline
description: Keep compatible applications on one endpoint and recover eligible requests through another approved node when the first fails before streaming begins.
slug: node-failover
image: /media/use-cases/node-failover-v1.webp
og_image: /media/use-cases/node-failover-og-v1.png
image_alt: Concept illustration of a connection moving from an unavailable model server to an available node.
persona: Platform & DevEx teams
icon: nodes
order: 5
---
# Recover when a model node goes offline

## A day-to-day case

A model machine is restarted for an update just as an application sends a request. Another approved machine has the model, but the application is still pointed at the first server.

You want to recover that request without asking every user to change an endpoint, relax a privacy rule or accept an unannounced model substitution.

## With Stoke

Configure eligible model destinations behind one gateway address. Stoke tracks node health and excludes unreachable nodes. On supported failover paths, a failed connection can be tried against another configured eligible destination before streaming begins.

For example, two approved nodes serve the same requested model. If the first cannot connect, the second can serve the request while the application keeps its existing endpoint. Route restrictions still apply: a private-only request must not escape to a cloud provider just to avoid an error.

## Know the boundary

This is connection-level recovery, not seamless mid-stream recovery. A response interrupted after streaming starts may still fail. Not every upstream error is eligible for another attempt, and no fallback can help if every permitted destination is unavailable.

Do not assume a smaller or different model is equivalent. Configure and test intended destinations, preserve the requested model where required, and disclose any explicitly permitted substitution. Recovery is not a promise of zero downtime or a published latency improvement.

## Try this setup

Use two approved nodes with the intended model and one compatible application. Make one node unreachable before a test request. Confirm the successful destination, inspect the returned route evidence and verify that no forbidden provider was called.

Then interrupt a response after streaming starts so the application's failure behavior is understood rather than mistaken for transparent recovery.

[Configure a two-machine inference setup](/guides/run-ollama-across-two-machines/)

[Review routing and fallback scope](/gateway/)
