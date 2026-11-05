---
title: A model host went offline. Now what?
description: A teammate closes the laptop serving your model. Give eligible requests another approved destination without reconfiguring every app.
slug: node-failover
image: /media/use-cases/node-failover-v1.webp
og_image: /media/use-cases/node-failover-og-v2.png
image_alt: A connection moving from an unavailable model server to an available node.
persona: Platform & DevEx teams
icon: nodes
order: 5
---
# A model host went offline. Now what?

A teammate closes their laptop after a demo. It is also the machine the team's assistant has been calling. The next request fails, although another approved computer could serve the same model.

Pointing every app at a particular host makes that host everyone's problem. A shared gateway address gives you somewhere to handle the failure instead.

## Give the request another approved destination

Configure eligible model hosts behind Stoke. It tracks node health and excludes unreachable nodes. On supported paths, a failed connection can be retried against another eligible destination before streaming begins.

With the intended model on two approved nodes, the application can keep its existing gateway endpoint when one cannot connect. There is no need to ask every teammate to edit a server URL just because a laptop disappeared from the network.

## A fallback must still obey the original rule

A private-only request must not go to a cloud provider simply to avoid returning an error. If every permitted destination is unavailable, the correct outcome is a refusal, not an unannounced change of policy.

This is connection-level recovery, not seamless mid-stream recovery. Once a response is streaming, an interruption can still fail the request. A different or smaller model is not automatically a suitable replacement either; approve and test intended substitutions explicitly.

## Close the laptop before someone depends on it

Test with two approved hosts and the intended model. Make one host unreachable before sending a request, then inspect which destination actually served it and confirm that no forbidden provider was called.

Interrupt a second response after streaming starts. Your application should handle that failure deliberately, rather than expecting Stoke to resume an answer halfway through.

[Configure a two-machine model setup](/guides/run-ollama-across-two-machines/)
