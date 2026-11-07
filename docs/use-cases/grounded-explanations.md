# Reclaim your codebase with `/brief`

At the speed agents are moving, any dormant misunderstandings you have with your human or
machine collaborators get amplified much more quickly than before. Agents also aren't bound
by [Miller's law](https://en.wikipedia.org/wiki/The_Magical_Number_Seven,_Plus_or_Minus_Two)
in the same way we are. They happily juggle different terminology for the same concept, and
this proclivity for inventing new names leaks into your codebase at every level of
granularity. As a result, your mental model of the system ends up drifting further and
further from what is actually implemented. Reclaiming a sense of understanding takes a
surprising amount of effort.

Re-aligning mental models requires using the same name for things that relate in the same
way:

1. Re-establish shared terminology: what concepts are we talking about?
2. Validate relationships: how do these concepts relate in the codebase?
3. Detect mis-alignment: where do nominally identical concepts relate in different ways?
4. Take corrective action, either by updating your mental model or by instructing your
agents to change the code to match it.
5. Rinse and repeat.

Lets see what that looks like, using the autoground codebase as an example.
