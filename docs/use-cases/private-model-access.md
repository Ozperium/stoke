---
title: Connect apps without copying provider keys
description: The next internal assistant needs model access, not another copy of an upstream credential. Keep provider keys at the gateway.
slug: private-model-access
image: /media/use-cases/private-model-access-v1.webp
og_image: /media/use-cases/private-model-access-og-v2.png
image_alt: A central credential kept behind a gateway with separate application access passes.
persona: IT & internal AI teams
icon: shield
order: 4
---
# Connect apps without copying provider keys

Your internal document assistant works. Now a developer wants to connect a coding tool, and someone else has a reporting script that could use the same model.

You can paste the provider key into each application and remember all those copies when it changes. Or give the applications a gateway credential and keep the upstream secret in one place.

## Let the app call Stoke, not the provider directly

Keep the model service behind a self-hosted Stoke gateway. Compatible applications use its endpoint and authenticate with accepted gateway credentials. If the upstream provider needs a key, the gateway holds it; the client does not need its own copy.

You can change eligible backends without changing every application's endpoint. Distinct configured client keys also let you separate supported metered budgets and rate limits, rather than making every experiment spend against the same allowance.

## A separate key is not a separate tenant

This is gateway authentication, not per-user RBAC. Separate keys do not automatically limit each caller to one model or route, and they do not create complete tenant isolation.

A gateway key still needs secure distribution and an offboarding process. Sharing permitted model or API access is not subscription lending: this setup does not turn personal subscription accounts into a shared team plan.

## Onboard one app, then remove its access

Connect a test application with its own configured gateway key. Confirm that it works without an upstream provider key and that an unauthenticated request is refused.

Then remove the test key from accepted configuration, restart the test gateway and confirm the app can no longer call it. Doing that once is more useful than assuming you'll know how to revoke access when it matters.

[Put an authenticated gateway in front of your model server](/guides/expose-ollama-to-your-network-safely/)
