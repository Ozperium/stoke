---
title: Share models, not provider keys
description: Give internal applications one authenticated path to private models, keeping upstream provider credentials at the gateway instead of copying them into every client.
slug: private-model-access
persona: IT & internal AI teams
icon: shield
order: 4
---
# Share models, not provider keys

## A day-to-day case

A team has a private model server and several applications that need it. One is a development assistant; another summarizes internal documents. Connecting each app directly means repeating credentials and network configuration every time a backend changes.

The operator wants a controlled front door, not an unprotected inference server or another copy of a provider secret in every application.

## With Stoke

Keep the model service on the trusted side of a self-hosted gateway. Compatible applications call the Stoke endpoint and authenticate with accepted gateway credentials. When the upstream provider needs its own key, keep that credential at the gateway rather than distributing it to clients.

Distinct configured client keys let the operator separate metered budgets and rate limits where supported. Applications retain the gateway address when eligible backends change. Review the configured routes and network access before making the endpoint available to the team.

## Know the boundary

This is gateway authentication, not per-user RBAC or complete tenant isolation. Separate keys do not automatically restrict each caller to one model or route. A gateway credential is still a secret: distribute it securely and remove it from accepted configuration when access must end.

This case covers models and API-backed access you are permitted to provide, not subscription lending. It does not promise SSO, team invitations, automatic per-user permissions or a publicly exposed model server that needs no network hardening.

## Try this setup

Connect one existing compatible application with a dedicated test gateway key. Confirm an unauthenticated request is refused and the application does not need the upstream provider key. Remove the test key from accepted configuration, restart the test gateway and confirm subsequent requests are refused.

[Connect an inference server without leaving it unprotected](/guides/expose-ollama-to-your-network-safely/)

[Review the self-hosted gateway capabilities](/gateway/)
