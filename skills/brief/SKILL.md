---
name: brief
description: Document, validate, and visualize conceptual models of any aspect of a codebase the user is interested in. Use when the user runs /brief.
argument-hint: "[instructions]"
disable-model-invocation: true
compatibility: Requires the autoground CLI on PATH.
license: MIT
---

At the speed agents are moving, any dormant misunderstandings between collaborators get amplified much more quickly than before. Different terminology for the same concept leaks into the codebase at every level of granularity. As a result, everyone's mental model of the system ends up drifting from the conceptual model that is actually  implemented.

Help the user realign their mental model with the current state of the codebase, by creating a `brief` (explained below) according to their instructions. Users will instruct about the concepts and relationships they want captured in the brief. Unless explicitly asked, don't include information about other concepts or relationships that exist in the codebase.

Use the `autoground check` utility to validate your brief. Iterate until all claims are verified. A detail is verified when its body is. If you notice that a claim made by the user is incorrect, report this misalignment back to them. Remember, the goal is not to get your brief to pass, but to accurately capture the state of the codebase. If you are struggling to get a brief accepted, recheck your own assumptions.

Once you have a valid brief, follow the users instructions as to how best to present it. Default to presenting a mermaid diagram embedded in a self-contained HTML file. Use `autoground align` to validate that the diagram accurately reflects the brief. Iterate on the diagram until it is accepted. Again, the goal is not to trick the judge, its to build an understanding of reality.


## Guidelines for writing an effective brief

Refer to the `references/` directory for documentation of the brief language.

The whole point of a brief is to iteratively align on a shared mental model. Aligning mental models requires using the same name for things that relate in the same way. Consistent terminology throughout is essential. Always default to using actual names from the codebase. Always use the same name to refer to the same concept. If you notice the user using a different name for a concept, point it out and ask for clarification.

The human brain pays a tax for every name it has to hold in working memory. If a brief grows past ten names, see if you can collapse some of the details into `(detail)` scopes. Identify separate themes in the brief and check with the user if they'd like to focus on a particular one. 

Sometimes it is unavoidable to introduce a new name, to refer to a concept that does not have a clear existing name in the codebase, or to talk about a real concept at a higher level, such as referring to specific data structures as "messages" or "state". Refer to the documentation, which explains how to use nullary senses to capture such definitions.

Strongly prefer using source files as witnesses. Derived documentation in e.g. markdown files should only be used cautiously, as it may not accurately reflect the current state of the codebase. Best to call it out to the user if a non-source witness is needed.

If `autoground check` doesn't accept your brief, break down your claims into smaller, more atomic propositions combined via `and`, `or`, or `not`. Use the opportunity to double check your own understanding. If you notice that you have to do unreasonable things to "trick" the judge, just surface that back to the user. The goal is building real understanding, not passing the check at all cost.

Never use `assume` when making claims about the codebase, always use `claim` with a witness. `assume` is appropriate for user-provided principles. If you have to introduce an assumption to get a brief to validate, flag it with the user explicitly, as it may point out a gap in their understanding.

Lastly, `autoground check` is not perfectly deterministic. A robust brief with the occasional near miss is expected and acceptable.
