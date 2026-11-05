---
title: Redact recognized secrets before sending
description: Keep an approved cloud workflow useful while opt-in, pattern-based filtering removes recognized secrets from supported message content before forwarding.
slug: secret-redaction
image: /media/use-cases/secret-redaction-v1.webp
og_image: /media/use-cases/secret-redaction-og-v1.png
image_alt: Concept illustration of document content preserved while selected lines are covered by redaction bars.
persona: Engineering & support teams
icon: filter
order: 6
---
# Redact recognized secrets before sending

## A day-to-day case

A support engineer wants a cloud model to help explain an error log. The error is useful context, but the pasted text may also contain an email address or a recognizable credential.

Keeping the entire task private is one option. When cloud use is already approved for this material, removing recognized secret patterns can be an additional safeguard without discarding the surrounding troubleshooting context.

## With Stoke

Enable the built-in redaction filter on the supported route and configure the patterns appropriate to the workflow. Before the request is forwarded, matching values in inspected message content are replaced with labeled redaction markers.

In a synthetic example, an email address in a log becomes an email redaction marker while the error text stays available to the model. The request continues to the permitted destination; this is different from a private-route policy that refuses a destination altogether.

## Know the boundary

This is pattern-based filtering, not complete DLP, anonymization or permission to send otherwise confidential data. It can miss sensitive information, and even redacted surrounding text may identify a person or disclose a company secret.

Verify the exact protocol, route and payload fields before relying on the filter. Do not assume that attachments, tool arguments, responses, application logs or every nested field receive the same inspection. Redaction can also remove text needed to solve the task.

## Try this setup

Use synthetic logs, never real credentials, in an isolated test. Include multiple known matches as well as nonmatching examples. Inspect the actual upstream payload and record both missed matches and unnecessary removals. Confirm that the sanitized request still supports a correct troubleshooting answer.

If the task cannot tolerate residual disclosure, use an approved private route instead of relying on redaction.

[Understand the request-filtering integration](/guides/block-prompt-injection-at-the-gateway/)

[Compare redaction with a private-data boundary](/use-cases/private-data/)
