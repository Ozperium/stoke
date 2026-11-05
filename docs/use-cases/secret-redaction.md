---
title: The error log came with an API key
description: Keep useful troubleshooting context while filtering recognized secrets from supported message content before an approved cloud request.
slug: secret-redaction
image: /media/use-cases/secret-redaction-v2.webp
og_image: /media/use-cases/secret-redaction-og-v3.png
image_alt: A timeout error stays in the log while Stoke replaces a dummy API key before forwarding.
image_layout: editorial
persona: Engineering & support teams
icon: filter
order: 6
---
# The error log came with an API key

The support ticket has the stack trace you need. It also has a customer email address and a credential someone copied from their environment. You want help understanding the error, not another destination holding that credential.

If the material is already approved for a cloud workflow, stripping recognized secret patterns adds a check before the model request leaves. If the material is confidential as a whole, use a private route instead.

## Keep the error, replace the recognized secret

Enable Stoke's redaction filter for the supported workflow and configure the patterns it should inspect. Matching values in supported message content are replaced with labeled markers before forwarding to the permitted destination.

In a synthetic log, that can mean keeping the timeout and stack trace while replacing a recognized email address. Unlike a private-route restriction, the sanitized request still continues to its approved model.

## The surrounding text can still give the secret away

Pattern-based filtering is not complete DLP. It can miss sensitive information, and a company name, internal URL or unique incident description may be enough to identify the customer without an email address.

Verify the protocol and fields you actually use. Do not assume attachments, tool arguments, responses or the application's own logs receive the same inspection. If residual disclosure is unacceptable, redaction is the wrong boundary for this task.

## Check what the upstream model receives

Use synthetic logs and dummy values in an isolated test, never real credentials. Include known matches, nonmatching text and context you need preserved. Inspect the actual upstream payload for missed matches and unnecessary removals.

Then check the answer. Removing the secret is useful only if the remaining context still lets the model help with the error.

[Review request-filtering integration](/guides/block-prompt-injection-at-the-gateway/)

[Use a private destination for confidential material](/use-cases/private-data/)
