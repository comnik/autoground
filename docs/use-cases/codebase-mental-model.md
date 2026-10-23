# Reclaim your codebase with `/brief`

`autoground` helps you (re-)establish and maintain an accurate mental model of a codebase.

At the rapid pace of agentic development, you can often feel your understanding of a
codebase — your mental model — drifting further and further away from ground truth. And even
though your agents excel at digging up information and answering your questions about the
implementation, reclaiming a sense of understanding takes a surprising amount of effort.

At the speed agents are moving, any dormant misunderstandings you have with your human or
machine collaborators gets amplified much more quickly than before. Agents also aren't bound
by [Miller's law](https://en.wikipedia.org/wiki/The_Magical_Number_Seven,_Plus_or_Minus_Two)
in the same way we are. They happily juggle different terminology for the same concept, and
this proclivity for inventing new names leaks into your codebase at every level of
granularity. As a result, your mental model of the system ends up no longer aligning with
the conceptual model that is actually implemented.

The process you are likely running to re-align the two is:

1. Re-establish shared terminology: what concepts are we talking about?
2. Validate relationships: how do these concepts relate in the codebase?
3. Detect mis-alignment: where do nominally identical concepts relate in different ways?
4. Take corrective action, either by updating your mental model or by instructing your
agents to change the code to match it.
5. Rinse and repeat.

In doing so, you've become what you fear most: the human in the loop slowing everything
down. And by the time that you're done the codebase has already changed out from under you.

With `autoground`, your agents document the outcomes of the above process in `brief`s, that
can then be validated against the current state of the codebase via the `autoground check`
utility. Validation is fast and cheap, often completing in seconds and for fractions of a
cent.

Aligning mental models requires using the same name for things that relate in the same way,
so consistent naming is the biggest lever for preserving understanding. `brief` is a
language for establishing shared terminology.

This pattern is packaged up in the [`/brief` skill](../../skills/brief/SKILL.md).
