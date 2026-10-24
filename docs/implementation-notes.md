## Implementation Notes

### Concepts

**Witness:** any piece of context that bears on a proposition: a file in a codebase, a
screenshot, a web resource, or really any other serializable information. The witness can
entail or contradict a proposition, or be neutral towards it.

**Universe:** the environment in which the witnesses live. Currently always a filesystem.

**Oracle:** the fundamental operation in `autoground` is determining how a witness $\Gamma$
bears on a proposition $p$. Does it support ($\Gamma \vdash p$) or contradict ($\Gamma
\nvdash p$) it? Or is the witness irrelevant and no conclusion can be drawn? Propositions
are in natural language, and so can be imprecise, handwavy, or subjective. We cannot rely on
traditional formal proof kernels. Instead, we use a [system one
model](https://docs.typesafe.ai/concepts/system-one) as a classification *oracle* $\Omega$,
a sort of neural proof kernel:

$$ \Gamma(p) := \Omega((\Gamma, p), \lbrace \text{entailed} \mid \text{contradicted}
\mid \text{neutral} \rbrace) $$

**Plan:** the oracle performs best when it is judging an atomic claim. The `brief` language
supports operators like `and`, `or`, and `not`, to decompose complex claims. A *plan*
encodes how to evaluate the overall brief from its atomic claims.

### Interfaces and components

**Log**: a unified log / session trace available to all components. Any `autoground` run
keeps a log in `/tmp/autoground`. The log interface allows all other components to append
their own structured events, as JSON lines, in a thread-safe way.

**Universe**: where a witness lives. `stat` and `read`. The local filesystem is the implementation.

**Oracle**: a classification oracle. Jev is the first implementation. Callers submit
classification tickets that they can wait on. Oracle implementations are free to batch and
paralellize pending tickets. Report usage statistics like spend, input tokens, request
count, latency percentiles, mean batch size (questions per request / max questions per
request), and mean fill factor (input tokens / the 32k state-side cap).

**Check**: takes the compiled claims and an oracle. Each witness is judged and the rest of
the plan evaluated deterministically.

### jev best practices

- Bias towards asking more questions over the same state. Really, really decompose.
- Break questions down into the smallest semantic unit.
- Leverage structured inputs. There should be no need for string templating.
