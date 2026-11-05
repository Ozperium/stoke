---
title: Put limits around agent spend
description: Bound metered API exposure for unattended agents with pre-dispatch budget checks, rate limits and loop protection, without relying only on billing alerts.
slug: agent-spend
persona: Agent builders & agencies
icon: gauge
order: 3
---
# Put limits around agent spend

## A day-to-day case

An agent is working while you are away. A failed attempt turns into repeated model calls. By the time you inspect the provider dashboard, the requests have already happened.

For a developer or agency running client work, the question is practical: how much metered API exposure are you willing to allow before another request should stop?

## With Stoke

Give a project its own configured gateway key and metered budget. Stoke checks admission before provider dispatch, reserving estimated maximum request cost against the allowance so concurrent requests cannot all spend the same remaining balance.

Rate limits bound request frequency. Loop detection can refuse repeated traffic with an explanation. When a request cannot be admitted, the application receives a refusal rather than another provider execution.

Use the reason and usage record to decide whether to repair the agent, adjust an approved limit or stop the run. A refusal is a boundary, not a successful task completion.

## Know the boundary

Dollar budgets apply to metered API usage, not subscription allowances. Configure model prices and output bounds correctly; cost estimates are not a reconciled provider invoice. Test the exact protocol and streaming behavior used by your application.

Loop detection can also interrupt legitimate repeated work. Tune and evaluate the overall workflow rather than treating fewer calls as automatic savings. Stoke does not stop shell commands, browser actions or external API charges outside its model-request path.

## Try this setup

Use a test key and a small metered budget. Submit concurrent requests and a controlled repeated-request canary. Inspect refused requests, provider dispatches and recorded usage, then confirm that normal work still completes correctly.

[Set a budget for Claude Code API usage](/guides/cap-claude-code-api-spend/)

[Stop a runaway AI agent](/guides/stop-a-runaway-ai-agent/)
