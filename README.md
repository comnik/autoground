# autoground

`autoground` compiles and runs *self-grounding* and *self-validating* conceptual models of a
codebase.

**Self-grounding** makes it possible for the model to be written as an unordered collection
of logical assertions over EDB that are not fully defined and not tied to the actual
codebase yet:
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

**Self-validating** simply means that all the IDB definitions in the model are evaluated
deterministically.

### Northstar

Express a conceptual model in an S-expression based modeling language, without adherence to
any rigid schema, just internally consistent.

If all constraints hold, then users can deterministically extract the grounding (e.g.
abstract component `x` in the model corresponds to `class XYZManager` in the code).

If any constraint is violated, then users can deterministically extract what part of the
model couldn’t be grounded.

### Principles

*Note: these all need to be pressure tested still.*

0. No baked-in assumptions about schema.
1. Deterministic IDB and constraints.
2. LLM-augmented EDB (pure LLM extract or LLM-selected deterministic grounder).
3. Top-down evaluation to guide which parts of the codebase to include in-context.
4. Stateless, optionally with EDB cached per revision, like a build artefact.

### Implementation notes

We should consider DuckDB as the storage and underlying execution engine.

We should consider using Jev (https://docs.typesafe.ai/) for fast parallel judgements, before
falling back to LLMs.

We should support an extensible set of deterministic grounding approaches as well, such as a
dependency analysis, CodeQL, (sem-)grep. When grounding the model we could have Jev decide
whether the atom is best supplied by a deterministic grounder or needs to be LLM-extracted.

Its not a requirement to use top-down evaluation, but it seems sensible, because it allows
us to refine where to spend effort grounding.