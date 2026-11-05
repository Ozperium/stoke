---
title: Stop funding an agent’s retry loop
description: A coding agent can keep paying to revisit the same error. Put a metered budget and loop protection in front of its next model request.
slug: agent-spend
image: /media/use-cases/agent-spend-v1.webp
og_image: /media/use-cases/agent-spend-og-v2.png
image_alt: A mechanical usage gauge stopped at a spending boundary.
persona: Agent builders & agencies
icon: gauge
order: 3
---
# Stop funding an agent’s retry loop

You leave an agent fixing a failing test while you do something else. The next attempt fails too. If the agent keeps asking the model for another explanation, how long should it be allowed to keep spending?

A billing alert can tell you what happened. For unattended work, you also need a point where the next paid request does not happen.

## Give the project an allowance before you leave

Use a dedicated gateway key with a configured budget for metered API calls. Before dispatch, Stoke reserves the estimated maximum request cost against that allowance. Concurrent requests cannot all claim the same remaining balance.

Rate limits control request frequency. Loop protection can refuse repeated model traffic. The caller receives a refusal when a request is not admitted, giving you a place to investigate instead of silently approving another provider execution.

## Stop the loop, then decide whether it deserves another attempt

A refusal does not mean the agent finished the task. It means you can inspect the reason, repair the workflow and deliberately approve more work if needed.

Set model prices and output bounds correctly. The allowance depends on those estimates; it is not a reconciled provider invoice. Dollar budgets cover metered API usage, not subscription allowances, shell commands or charges from tools outside Stoke's model-request path.

## Check that useful repetition still works

Start with a small test allowance. Submit concurrent requests and a controlled repeated-request canary, then inspect provider dispatches and refusals. Also run ordinary work through the same setup: an agent that legitimately revisits a problem should not be mistaken for a runaway loop.

The useful outcome is a task completed within an approved allowance, not merely fewer model calls.

[Cap Claude Code API spend](/guides/cap-claude-code-api-spend/)

[Test a runaway-agent stop](/guides/stop-a-runaway-ai-agent/)
