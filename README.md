# ⏚ autoground

`autoground` compiles and runs *neurosymbolic*, *self-validating* mental models of a
codebase.

**Neurosymbolic**, because models are written as unordered collections of logical
propositions. But those propositions can be "handwavy" — they don't need to adhere to any
fixed schema, can reference abstract concepts, be imprecise or qualitative, and generally
don't need to be aware of any specifics about _how_ the codebase is implemented:

```
(sense calls "{caller} calls {callee}")
(sense contains "{X} contains {Y}")

(name Oracle)
(name Universe)
(name Search)
(name Read)

;; Claims

("An Oracle is a general classifier leveraging a system-one model like jev.")

("autoground emits a log of structured events")

(not (calls Universe Oracle))

(and
  ("Search prunes a Universe to a window of relevant objects along with a signal score.")
  (calls Search Universe)
  (calls Search Oracle))
```

**Self-validating** simply means that all claims in the model are evaluated and so can catch
misconceptions and violated assumptions.

## North Star

A developer can express their mental model of a heavily vibe-engineered codebase in a simple
S-expression language, without adherence to any rigid schema. `autoground` will run it and
return either *entailed*, *contradicted*, or *unsupported*, indicating whether the model is
both internally consistent and grounded in the reality of the codebase.

Each response includes provenance: the databases in the codebase supporting or contradicting
base facts, and the logical deductions that were performed on top.

## Concepts

### Universes

We are working with a fixed *context window* of size $C$ and are interacting with a finite
*universe* of *databases* $\mathbf{U} = \lbrace \Gamma_1,...,\Gamma_n \rbrace$. While the
overall universe can be large, each database is assumed to be serializable, and for that
serializable representation to fit into a single context window, i.e. $|\Gamma_i|<C$.

Additionally, we assume that the databases are arranged in a tree. This hierarchy lets us
consume the universe at different *levels of detail*. Databases higher up in the tree
abstract over their subtrees.

Concretely, the particular kind of universe we care about for now is a codebase — a set of
files (the leaf databases) arranged in a hierarchy (the repository's directory structure).
But other practically relevant data structures fit this pattern: an agent trace grouped by
tool-calling cycle, or a Slack archive with messages grouped into threads and channels.

### Neural Evaluation

The most fundamental operation that we need to support is a user giving us a proposition
$p$, e.g. a fact they are asserting about their codebase. We need to determine whether the
given universe supports ($\mathbf{U} \vdash p$) or contradicts ($\mathbf{U} \nvdash p$) the
user's proposition, or whether no conclusion can be drawn. We refer to the process of
computing $\mathbf{U}(p) = \lbrace \text{true} \mid \text{false} \mid \text{unknown}
\rbrace$ as *evaluation*.

Because user propositions can be imprecise, abstract, or qualitative, we cannot rely on
traditional deterministic methods for evaluation. Instead, we use a [system one
model](https://docs.typesafe.ai/concepts/system-one), which will simply refer to as the
*oracle* $\Omega$. We can translate the evaluation of any proposition into a classification
problem for the oracle

$$ \mathbf{U}(p) := \Omega((\mathbf{U}, p), \lbrace \text{entailed} \mid \text{contradicted}
\mid \text{neutral} \rbrace) $$

The oracle, however, is limited to a single context window worth of input for any single
call. To work around this limitation, we effectively do a oracle-guided *search* to find
relevant context windows, followed by a map-reduce-like evaluation on all of those.

#### Search

Search leverages the oracle to construct a candidate view: the paths in the universe that
should be explored in order to evaluate a proposition. For every path, it asks the oracle
whether that path is *irrelevant* to the given proposition. Only a confident "irrelevant"
results in the path getting pruned from the candidate view. This keeps search conservative,
while giving us a relevance signal in which to judge the remaining files. Because of this
conservatism, the candidate view can be interpreted as a closed world, allowing the reader
to short-circuit. Search never reads file contents.

#### Read

`autoground read` submits a proposition, searches, then judges the leaves of that view.
`submit`
enqueues and `wait` flushes every pending proposition together, the same batching shape as
search and the oracle.

Files are judged in order of relevance (as determined by the search step). All propositions
pending for a given file are packed into the same request.

Each body is a choice of entailed, contradicted, or neutral. A confident "entailed"
short-circuits to true, a confident "contradicted" to false. Weak judgements are kept until
there is a conflict or the window runs out. Conflicting judgments are escalated once more to
the oracle to resolve. Because search is conservative, we make a closed world assumption.
Exhausting the candidate view with no witness is false.

Returns the truth value plus the evidence view: the databases the value depends on.

### The `lect` Language

`autoground` is ultimately a tool for (re-)establishing shared context between the author
and the reader of a codebase. Shared context is:

1. Shared ontology. (<- our current focus)
2. Shared definitions.
3. Shared constraints.
4. Shared motivation.

Aligning ontologies means using the same name for things that relate in the same way.
Consistent naming is the biggest lever for keeping complexity in check. This is what the
`lect` modeling language is meant to help with.

A model consists of *names*, *senses*, and *claims*:

```lect
;; A namespace is the name declared here. This file can live anywhere
;; in the repository.
(ns autoground.examples
  (:require [autoground.core :as core]
            [autoground.names :refer [Oracle]]
            [autoground.api :refer :all]))

;; Names
(name X)
(name Y)

;; Senses are readings of a relationship, potentially with holes.
("X has an identifier")                             ;; nullary, unnamed
("{Name} has an identifier" X)                      ;; unary, unnamed
("{Name} has a {Name}" X foo)                       ;; binary, unnamed
(sense component "{Name} is a part of the system")  ;; unary, named
(sense part_of "{Name} is a part of {Name}")        ;; binary, named

;; Roles name the holes on a sense, to disambiguate.
(sense property "{object} has a {property}")
(sense borrowed_from "{borrower} borrowed from {lender}")

;; Claims are logical propositions without quantifiers.
(component X)
(part_of X Y)

(and
  (component X)
  (component Y)
  (depends_on X Y))

(or
  (part_of X Y)
  (part_of Y X))

(not (depends_on X Y))
```

Wherever a `.lect` file is accepted, a `.md` file is too: every fenced `lect` block is
taken, in order, and the rest of the document is left out. `(:require ...)` finds another
namespace by the name it declares. A `.lect` file can sit anywhere in the repository.

The filesystem universe implementation filters out `.lect` files to make sure unvalidated
claims are never treated as evidence.

### Verbalization

TBD verbalization is how we go from symbolic atoms to unstructured propositions.

```clojure
;; Fully ground senses verbalize as themselves.
("X has an identifier") ;; => "X has an identifier"

(sense component "{Name} is a part of the system")
(sense part_of "{Name} is a part of {Name}")

(component X) ;; => "X is a part of the system"
(part_of X Y) ;; => ("{Name} is a part of {Name}" X Y) => "X is a part of Y"
```

### Symbolic Evaluation

`autoground plan` decomposes a model into positive atomic sentences. It does not
render a file body and does not call the reader. `autoground run` reads those
sentences and combines the findings with Kleene `and`, `or`, and `not`.


## Implementation

### Milestones

- [x] `autoground search "<arbitrary verbalized proposition>"`
- [x] `autoground read "<arbitrary verbalized proposition>"`
- [x] `autoground plan xyz.lect`: list the atomic sentences in a model
- [x] `autoground run xyz.lect`: read those sentences and combine them per claim

### Interfaces

**Log**: a unified log / session trace available to all components. Any `autoground` run
keeps a log in `/tmp/autoground`. The log interface allows all other components to append
their own structured events, as JSON lines, in a thread-safe way.

**Universe**: a view over the universe.
- compute the size of the current view (a summary's length, or the content length of a database that has no summary; bodies are not read)
- read one database that has no summary
- sampling from the next level of detail to provide additional context
- methods to add and remove databases from the view explicitly, based on oracle classification
- whether a database has a summary

As the README hopefully makes clear, the universe abstraction would treat files and
directories as the same thing — a "database". Files would be the leaf databases in the tree.
A database with a summary is classified from that text. A database without one is read.

**Oracle**: a classification oracle. Jev is the first implementation.
- submit a classification `(state, proposition, labels)` as soon as a need is known
- wait for a ticket when the label is required; pending questions that share a `state` go in one request, independent states overlap
- report usage on this instance so far: spend, input tokens, request count, latency percentiles, mean batch size (questions per request / max questions per request), and mean fill factor (input tokens / the 32k state-side cap)

The oracle hashes the `state` it is given (a listing tree, a file body). Universe itself has no digest method. Search and the reader use this interface. The 3VL engine does not hold an oracle.

**Search**: takes a universe, an oracle, and a natural language proposition, returns the
pruned universe that is relevant to answering the proposition. Same auto-batching behaviour
as the oracle: multiple search requests can be submitted that then get batched up
appropriately.

**Reader**: takes a universe, an oracle, and a natural language proposition, returns a truth
value and the evidence view inside the search window. Same auto-batching: `submit` enqueues;
`wait` flushes every pending proposition so file bodies are shared across propositions.

### jev best practices

- Leverage structured inputs. There should be no need for string templating.
- Bias towards asking more questions over the same state. Really, really decompose.
- Break things down into the smallest semantic unit.

### Notes

No baked-in assumptions about schema.

Three-valued logic, so we can distinguish "entailed", "unsupported", _and_ "contradicted".

Neural EDB / symbolic IDB.

Top-down evaluation to guide which parts of the codebase to include in-context.

Stateless, optionally with EDB cached per revision, like a build artefact.

We'll use jev (https://docs.typesafe.ai/) as the system one model for now, behind an
abstract oracle interface.

For memoization and symbolic evaluation, we should consider DuckDB.

Its not a requirement to use top-down evaluation, but it seems sensible, because it allows
us to refine where to spend effort grounding.

Classification tasks are phrased in terms of entailed / contradicted / neutral; evaluation
uses true / false / unknown.