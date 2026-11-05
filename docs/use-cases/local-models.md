---
title: Use models across your machines
description: One endpoint for developers and small teams using local models across their own computers. Keep model access steady as available machines change.
slug: local-models
image: /media/use-cases/local-models-v1.webp
og_image: /media/use-cases/local-models-og-v1.png
image_alt: Concept illustration of several personal computers using a shared model connection.
persona: Developers & small teams
icon: nodes
order: 1
---
# Use models across your machines

## A day-to-day case

Your workstation has the model you need. Your laptop is where you work. A second machine has spare capacity, but switching every application between endpoints turns that capacity into another thing to manage.

You want one place to send model requests, without moving the whole agent or development environment to another computer.

## With Stoke

Connect compatible applications to one gateway endpoint. Configure the machines you own as inference destinations, directly through Ollama or through another Stoke gateway.

Stoke checks eligible models and node health, prefers warm capacity, and uses live load when choosing where to send a request. Separate requests can use separate machines. Where supported, another permitted node can serve a request when the first is unavailable before streaming begins.

Your application keeps the gateway address. You keep control of which models and destinations are eligible.

## Know the boundary

This is request routing, not combined GPU memory: several computers do not become one machine capable of loading a larger model. Better throughput depends on your actual models and workload; hardware and electricity still cost money.

A remote inference host can access the request it processes. Use machines and network access you trust. A private-only route must refuse an unavailable destination rather than silently use cloud inference. Do not assume interrupted streams can resume on another node.

## Try this setup

Start with two machines and one compatible application. Send a request through the same endpoint, then take one eligible node offline and inspect the outcome. Compare completed work and latency with your original single-node setup.

[Set up Ollama across two machines](/guides/run-ollama-across-two-machines/)

[Install the self-hosted gateway](/gateway/#install)
