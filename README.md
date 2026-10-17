# ⏚ autoground

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

`autoground` lets your agents document the outcomes of the above process in so called
`brief`s, that can then be validated against the current state of the codebase via the
`autoground check` utility. Validation is fast and cheap, often completing in seconds and
for fractions of a cent.


## The `brief` language

Aligning mental models requires using the same name for things that relate in the same way,
so consistent naming is the biggest lever for preserving understanding. `brief` is a
language for establishing shared terminology.

> **[brief](https://www.merriam-webster.com/dictionary/brief)**<br/>
> *"a formal written presentation of an argument that sets forth the main points with
> supporting precedents and evidence"*

A brief consists of *names*, *senses*, and *witnesses*:

```brief
;; A namespace globally disambiguates this brief.
(ns autoground)

;; Names identify concrete or abstract concepts. Components and data structures in the
;; codebase, mathematical objects, or things in the real world.
(name Frontend)
(name Backend)
(name Program)
(name AST)

;; Senses express a claim about a name, or the relationship between names.
;; Unambiguous senses can be written as plain text sentences.
Program is a data structure
AST is a data structure
Frontend accepts Program
Frontend returns AST
The system convers Program to AST via Frontend

;; The explicit syntax for a sense requires quotes and parentheses.
("Program is a data structure")
("Frontend accepts Program")

;; Senses can have free variables, in which case the claim is not fully formulated yet.
("{Name} is a data structure")                      ;; single free variable
("{Name} returns {Name}")                           ;; two free variables

;; Senses can be named as well, to not have to fully spell them out everywhere.
(sense is_data "{Name} is a part of the system")    ;; single free variable
(sense is_component "{X} is a part of the system")  ;; single free variable
(sense returns "{Name} returns {Name}")             ;; two free variables

;; Roles name the free variables on a sense, to disambiguate.
(sense returns "{Component} returns {Data}")
(sense property "{object} has a {property}")
(sense borrowed_from "{borrower} borrowed from {lender}")

;; A claim becomes verifiable when its tied to one or more witnesses.
(witness
  ("Sweep asks the oracle whether a path matches the filter")
  "src/sweep/mod.rs")

;; Atomic claims can be negated, or composed into conjunctions and disjunctions. 
(not (depends_on X Y))

(and
  (component X)
  (component Y)
  (depends_on X Y))

(or
  (part_of X Y)
  (part_of Y X))

;; Any claim that can't be validated as a whole, can be decomposed.
(detail (calls Sweep Oracle)
  (witness
    ("Sweep asks the oracle whether a path matches the filter")
    "src/sweep/mod.rs")
  (witness
    ("Sweep's oracle posts are the Jev HTTP call")
    "src/sweep/mod.rs"
    "src/oracle/jev.rs"))
```

The ordering of the top-level declarations is irrelevant.

Briefs are written to `*.brief` files. Wherever a `*.brief` file is accepted, a `.md` file
is too: every fenced `brief` block is taken, in order, and the rest of the document is left
out.

Briefs are not accepted as witnesses for other briefs, to make sure unvalidated claims are
never treated as evidence.


## `autoground check`

```
autoground check examples/search-calls-oracle.brief
autoground check examples/sqrt2.brief
```

Think of the `autoground check` utility as a natural language proof assistant.

Briefs compile to a set of inter-dependent claims, which `check` then validates starting
from the referenced witnesses. Claims are in natural language, and so can be imprecise,
handwavy, or subjective. We therefore cannot rely on traditional formal proof kernels.
Instead, we use a [system one model](https://docs.typesafe.ai/concepts/system-one) as an
oracle, a sort of neural proof kernel.

At every step in the proof the oracle is provided with the relevant hypotheses and is asked
to judge the proposed conclusion as "supported", "contradicted", or "neutral". This starts
with the files referenced by witnesses. Every `detail` is a deductive step. Only logical
operators are resolved determinisitically, without involving the oracle. Each unique claim
is only proved once, no matter how often it is referred to.

Exit status is 0 when every top-level claim is discharged, 1 otherwise, and 3 on a compile,
IO, or oracle failure. Each check writes a detailed trace of all proof steps along with the
requests sent to the oracle, a `context.md` containing the fully spelled out version of all
validated claims, and a `diagram.mmd` of the validated names and senses.


## `autoground sweep`

```
autoground sweep examples/no-historic-comment.brief --filter "source file"
```

`sweep` is the counterpart to `check`. Where `check` starts from specific files to validate
a claim, `sweep` scours the entire codebase to look for files that match an as-of-yet
unwitnessed claim.

Use `check` to continuously validate your mental model, and `sweep` to identify areas that
might deserve more attention.

Every matching file is printed along with the specific claims it matched. `sweep` also
writes a detailed trace file that records all calls to the oracle.

`sweep transcript` judges the tool calls and responses of an agent's session transcript.

```
autoground sweep transcript examples/no-historic-comment.brief chat_history.jsonl
```

`sweep diff` judges each change in a git diff.

```
autoground sweep diff examples/no-historic-comment.brief
autoground sweep diff examples/no-historic-comment.brief --cached
autoground sweep diff examples/no-historic-comment.brief HEAD~3..HEAD
autoground sweep diff examples/no-historic-comment.brief origin/main...HEAD -- src/
```


## Implementation

### Concepts

**Witness:** any piece of context that bears on a proposition: a file in a codebase, a
screenshot, a web resource, or really any other serializable information. The witness can
entail or contradict a proposition, or be neutral towards it.

**Universe:** `autoground sweep` can currently operate on codebases, agent session
transcripts, or git diffs. We refer to these generically as the *universe* $\mathbf{U}$.
Other implementations are conceivable.

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

**Universe**: a filterable view over the universe.

**Oracle**: a classification oracle. Jev is the first implementation. Callers submit
classification tickets that they can wait on. Oracle implementations are free to batch and
paralellize pending tickets. Report usage statistics like spend, input tokens, request
count, latency percentiles, mean batch size (questions per request / max questions per
request), and mean fill factor (input tokens / the 32k state-side cap).

**Sweep**: takes a brief, a universe, and an oracle. The filesystem universe takes an
optional filter, based on which the oracle prunes the listing. Every remaining file is
judged against the brief. A file matches when any top-level claim is true. All matching
files are reported along with the corresponding claims.

**Check**: takes the compiled claims and an oracle. Each witness is judged and the rest of
the plan evaluated deterministically.

```brief
The oracle hashes the `state` it is given (a listing of paths, a file body). Universe itself has no digest method. Sweep and check use this interface.
```

### jev best practices

- Bias towards asking more questions over the same state. Really, really decompose.
- Break questions down into the smallest semantic unit.
- Leverage structured inputs. There should be no need for string templating.
