# autoground

`autoground` compiles and runs *neurosymbolic*, *self-validating* mental models of a
codebase.

**Neurosymbolic**, because models are written as unordered collections of logical
propositions. But those propositions can be "handwavy" — they don't need to adhere to any
fixed schema, can reference abstract concepts, be imprecise or qualitative, and generally
don't need to be aware of any specifics about _how_ the codebase is implemented:

```
(require
  (and
    (component "Frontend")
    (component "Backend")))

;; A derivation rule defining the relationship `depends_on`.
;; Note how `file` and `includes` are undefined atoms.
(def depends_on [x y]
  (and
    (component x)
    (component y)
    (or
      (exists [f]
        (and
          (file f)
          (includes x f)
          (defines f y))))))

;; A relationship `depends_on` connecting the two components.
(require
  (depends_on "Frontend" "Backend"))

;; An integrity constraint asserting that the architectural layering must not be violated.
(require
  (not (depends_on "Backend" "Frontend")))

;; An integrity constraint asserting that there should be no circular dependencies.
(require
  (forall [x y]
    (implies
      (depends_on x y)
      (not (depends_on y x)))))
```

**Self-validating** simply means that all assertions in the model are evaluated and so can
catch misconceptions and violated assumptions.

## North Star

A developer can express their mental model of a heavily vibe-engineered codebase in a simple
S-expression language, without adherence to any rigid schema. `autoground` will run it and
return either *entailed*, *contradicted*, or *unsupported*, indicating whether the model is
both internally consistent and grounded in the reality of the codebase.

Each response includes provenance: the objects in the codebase supporting or contradicting
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

Search walks the universe coarse to fine and judges each database in the call that selects
it. The cost of a run is the number of oracle calls, so two bounds matter. If the children of
the root fit in one window, that frontier is one call: a small repository is not walked
directory by directory. If a directory listing fits and the bodies under it do not, the
directory can be dropped before any body is opened.

1. The frontier starts as the children of the root. The root is not a target.
2. Render the frontier at native grain: a directory as its listing, a file as its path and
   body. Split the frontier into chunks that fit. Trim a single oversized file. Replace a
   single oversized listing with its children.
3. Spend leftover budget on samples of the next level. Samples are context, not targets.
4. One Choice per target, sharing that rendered state.
   - A directory is $\lbrace \text{prune} \mid \text{refine} \mid \text{true} \mid \text{false} \rbrace$.
     `true` and `false` mean the current view settles the proposition — the presence of a
     subdirectory is enough for "the codebase has a frontend". There is no `unknown` for a
     directory. `refine` means the view is missing something required to decide.
   - A file is $\lbrace \text{true} \mid \text{false} \mid \text{unknown} \rbrace$. The body is
     the whole leaf, so this call is the evaluation. When those bodies fit in the frontier's
     budget they are judged in that same call, and a later `refine` does not fetch them again.
5. Record every directory judgement and every file judgement. Step into the children of each
   `refine`.

`prune` below the confidence bar is treated as `refine`. `true` or `false` below the bar is
`refine` on a directory and `unknown` on a file.

The universe has no collapse operation. Lifting every object to its parent mixes depths on an
unbalanced tree, and the bounds above do not need it. Search refines by stepping to children.

Deterministic strategies (CodeQL, grep) can later propose a frontier. They are not part of
this walk.

#### Map + Reduce

Search already records a judgement for every database it accepts. A directory is `true` or
`false` from its listing. A file is `true`, `false`, or `unknown` from its body. What remains
is to reduce those judgements:

$$
\text{reduce}(v_1, v_2) = \begin{cases}
    v_1 & \text{if } v_1 = v_2 \\
    v_1 & \text{if } v_2 = \text{unknown} \\
    v_2 & \text{if } v_1 = \text{unknown} \\
    \text{ESCALATE} & \text{otherwise } (v_1 \neq v_2)
\end{cases}
$$

Escalations result in a final call to the oracle, classifying the conflicting judgements
into $\lbrace \text{true} \mid \text{false} \mid \text{contradiction} \rbrace$.

### Symbolic Evaluation

TBD

### Verbalization

TBD verbalization is how we go from symbolic atoms to unstructured propositions.


## Implementation

### Milestones

- [x] `autoground search "<arbitrary verbalized proposition>"`
- [ ] `autoground eval "<arbitrary verbalized proposition>"`
- [ ] `autoground plan xyz.ag`: demonstrate the top-down demand transform + verbalization

### Interfaces

**Universe**: a view over the universe.
- estimate / compute the size of the current view of the universe (e.g. total size of all reachable files from the repo root)
- rendering the current view (e.g. rendering the current set of paths as a tree, concatenating files with header information, things like that)
- sampling from the next level of detail to provide additional context
- methods to add and remove objects from the view explicitly, based on oracle classification
- whether an object is a leaf (path plus contents) or a listing

As the README hopefully makes clear, the universe abstraction would treat files and
directories as the same thing — a "database". Files would be the leaf databases in the tree.
Rendering a file would render at least the full path + its contents. Rendering a directory
would render its listing.

**Oracle**: a classification oracle. Jev is the first implementation.
- submit a classification `(state, proposition, labels)` as soon as a need is known
- wait for a ticket when the label is required; pending questions that share a `state` go in one request, independent states overlap
- report usage on this instance so far: spend, input tokens, request count, latency percentiles, mean batch size (questions per request / max questions per request), and mean fill factor (input tokens / the 32k state-side cap)

The oracle hashes the `state` it is given (a Universe render, a listing tree, a file body). Universe itself has no digest method. Search uses this interface. The 3VL engine does not hold an oracle.

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