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
*universe* of *databases* $\mathbf{U = \{\Gamma_1,...,\Gamma_n\}}$. While the overall
universe can be large, each database is assumed to be serializable, and for that
serializable representation to fit into a single context window, i.e. $|\Gamma_i|<C$.

Additionally, we assume that the databases are arranged in a tree. This hierarchy lets us
consume the univser at different *levels of detail*. Databases higher up in the tree
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
computing $\mathbf{U}(p) = \{true, false, unknown\}$ as *evaluation*.

Because user propositions can be imprecise, abstract, or qualitative, we cannot rely on
traditional deterministic methods for evaluation. Instead, we use a [system one
model](https://docs.typesafe.ai/concepts/system-one), which will simply refer to as the
*oracle* $\Omega$. We can translate the evaluation of any proposition into a classification
problem for the oracle

$$
\mathbf{U}(p) := \Omega((\mathbf{U}, p), \{\text{entailed}, \text{contradicted}, \text{neutral}\})
$$

The oracle, however, is limited to a single context window worth of input for any single
call. To work around this limitation, we effectively do a oracle-guided *search* to find
relevant context windows, followed by a map-reduce-like evaluation on all of those.

#### Search

To understand the motivation behind the search algorithm, it is instructive to consider an
extreme case: if the full universe fits in a single context window, then evaluating a single
proposition should require only a single call to the oracle. For a small codebase, that
would mean handing the oracle all files concatenated together as context. Our overall cost
and latency is determined by the number of oracle calls we make, so any algorithm we come up
with should at least achieve this lower bound.

Another special case to consider is one where all non-leaf databases fit in context, but the
leaves don't. In that case, the search procedure should be able to prune at the highest
level of detail. For a codebase, this would mean pruning the set of all file paths,
regardless of where in the directory they sit.

This suggests a divide and conquer approach where we...

1. Start with all databases as candidates and the target proposition $p$.
2. Reduce the level of detail until the candidate set fits in context.
3. Fill up whatever space is left with samples from the next higher level.
3. Ask the oracle to classify each candidate pair $(\Gamma, p)$ into
   ${\{\text{prune}|\text{explore}|\text{candidate}\}}$. Remove the $\text{prune}$'s from
   the candidate set.
4. Recursively start a new search with the $\text{explore}$'s as seed candidates. Add the
   results to the global candidate set.

This leaves us with a final set of (leaf or non-leaf!) candidate databases. A non-leaf
database directly identified by the oracle as a candidate is not unusual. In the codebase
example, it could indicate that the mere presence of a sub-directory can already support or
contradict the user's proposition.

If the model is smart enough, then it should be a reasonable assumption that for two
propositions

$$
\forall{p_1, p_2}: (p_1 \implies p_2) \implies  \text{Search}(\mathbf{U}, p_1) \subset
\text{Search}(\mathbf{U}, p_2)
$$

Once we start evaluating more complex formulas, this could be exploited to eliminate
redundant searches or fold multiple searches into a single one.

Deterministic search strategies like CodeQL, (sem-)grep, and others can be integrated as a
built-in library, that the oracle can choose from.

#### Map + Reduce

With a set of candidate databases $\{\Gamma_1,...,\Gamma_n\}$ in hand, all we have to do is
to map the original evaluation

$$
\mathbf{\Gamma_i}(p) := \Omega((\mathbf{\Gamma_i}, p), \{true, false, unknown\})
$$

And then reduce the truth values as follows

$$
\text{reduce}(v_1, v_2) = \begin{cases}
    v_1 & \text{if } v_1 = v_2 \\
    v_1 & \text{if } v_2 = \text{unknown} \\
    v_2 & \text{if } v_1 = \text{unknown} \\
    \text{ESCALATE} & \text{otherwise } (v_1 \neq v_2)
\end{cases}
$$

Escalations result in a final call to the oracle, classifying the conflicting judgements
into ${\{\text{true},\text{false},\text{contradiction}\}}$.

### Symbolic Evaluation

TBD

### Verbalization

TBD verbalization is how we go from symbolic atoms to unstructured propositions.


## Implementation Notes

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