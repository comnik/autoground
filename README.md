# ⏚ autoground

autoground is an <ins>informal</ins> proof assistant.

Together with your favourite coding agent, autoground helps you to (re-)establish and
maintain an accurate mental model of a codebase that has drifted out from under you.

**use [`/brief`](docs/use-cases/codebase-mental-model.md) to validate your mental model of a
codebase**

**use [`/proof`](docs/use-cases/rigorous-reasoning.md) to construct a rigorously reasoned
argument**

Unlike formal proof assistants like Lean or Rocq, autoground uses a [system one
model](https://typesafe.ai/blog/introducing-system-one-models-and-jev) as its proof kernel,
which has all kinds of wacky implications. The kernel effectively accepts any deductive step
that a capable LLM can follow *intuitively*, meaning without the use of reasoning. The trust
boundary shifts: it becomes easier for you to verify assumptions and claims made throghout
the proof, but the informal and non-deterministic nature of the approach admits both false
positives and negatives.

Once the [uneasy feeling passes](https://en.wikipedia.org/wiki/Proofs_and_Refutations), this
can be quite fun! autoground proofs can be written in (mostly) natural language and with
witnesses drawn from different input modalities, which means you could prove that a given
piece of code produces the screenshot of a website you just took.


## Install

```shell
git clone git@github.com:comnik/autoground.git
cd autoground
make install
```

This will install the `/brief` and `/proof` skills, along with the autoground binary itself.

> [!NOTE]
> Using autoground requires a [TypeSafe API key](https://docs.typesafe.ai/introduction) to
> be available as `TYPESAFE_API_KEY` in your environment.

Set your TypeSafe key in the environment. Every use of autoground incurs API usage. By
default, all commands are capped at a budget of 1 cent.

<!--
```brief
(ns readme)

(claim "check runs with the default budget cap by default"
  (witness "src/main.rs"))

(claim "the default budget cap is 1 cent"
  (witness "src/oracle/jev.rs"))
```
-->

```shell
export TYPESAFE_API_KEY=apikey_...
```

Confirm that everything is working, by validating this README:

![README Demo](docs/assets/vhs/readme.gif)

Then learn about the skills:

- [`/brief`](docs/use-cases/codebase-mental-model.md): validate your mental model of a
  codebase
- [`/proof`](docs/use-cases/rigorous-reasoning.md): construct a rigorously reasoned argument


## The `brief` language

Informal proof is a dynamic process where proof author and verifier iteratively align their
respective mental models and expose implicit assumptions. Aligning mental models requires
using the same name for things that relate in the same way. brief is a language with two
goals: 1) help establish shared terminology, and 2) support gradual decomposition of a
complicated argument into increasingly atomic claims.

> **[brief](https://www.merriam-webster.com/dictionary/brief)**<br/>
> *"a formal written presentation of an argument that sets forth the main points with
> supporting precedents and evidence"*

A brief consists of *names*, *senses*, *claims*, and *witnesses*:

```clojure
;; A namespace globally disambiguates this brief.
(ns autoground)

;; Names identify concrete or abstract concepts. Components and data structures in the
;; codebase, mathematical objects, or things in the real world.
(name Frontend)
(name Backend)
(name Program)
(name AST)

;; Senses define templates for attaching meaning to names and for establishing
;; relationships between them.
(sense is_data "{Name} is a data structure")        ;; single free variable
(sense is_component "{X} is a component")           ;; single free variable
(sense returns "{Name} returns {Name}")             ;; two free variables

;; Note how the variables can be named as well, to disambiguate roles in a relationship.
(sense property "{object} has a {property}")
(sense borrowed_from "{borrower} borrowed from {lender}")

;; Senses instantiated with names can be used to make statements about the codebase.
(is_component Frontend)
(is_component Backend)
(is_data AST)
(returns Frontend AST)

;; The above claims are all unsupported and so can neither be validated nor contradicted.
;; A claim becomes verifiable when it is tied to a witness.
(claim (is_component Frontend)
  (witness "src/frontend/mod.rs"))

(claim "Check asks the oracle whether a witness supports the claim"
  (witness "src/check/mod.rs"))

;; Claims can be supported by other claims, forming a chain of deductions.
(sense lemma-1 "If a whole number is odd, then its square is not even.")
(sense lemma-2 "If the square of a whole number is even, then that number is not odd.")
(claim (lemma-2)
  (lemma-1))

;; Crucially, any claim that can't be validated directly from a witness, can be decomposed
;; into conjunctions and disjunctions of smaller claims via `not`, `and`, and `or`.
(claim (is_component Backend)
  (not (is_data Backend)))

(claim (returns Frontend AST)
  (and
    (is_component Frontend)
    (is_data AST)
    (claim "Frontend parses the program and returns AST"
      (witness "src/frontend/parser.rs"))))

(claim (related Frontend Backend)
  (or
    (depends_on Frontend Backend)
    (depends_on Backend Frontend)))
```

Note how applying names to a sense boils down to a fully formed sentence:

```clojure
   (returns Frontend AST)
=> ("{Name} returns {Name}" Frontend AST)
=> "Frontend returns AST"
```

There are two additional language constructs: *assume* and *intent*.

```clojure
;; Sometimes, we're just looking to make an internally consistent argument that is based
;; on a handful of explicit assumptions, rather than some observable ground truth.
(sense axiom "The square of an even whole number is even.")
(assume (axiom))

;; It is often helpful to capture the intent behind a brief. What do we want the
;; recipient to take away? What mental model are we trying to convey?
(intent "This brief is intended to convey the compilation pipeline.")
```

The ordering of the top-level declarations is irrelevant.

Briefs are written to `*.brief` files. Wherever a `*.brief` file is accepted, a `.md` file
is too: every fenced `brief` block is taken, in order, and the rest of the document is left
out.

Briefs are not accepted as witnesses for other briefs, to make sure unvalidated claims are
never treated as evidence.

Finally, briefs can be nested, to hide *details* of an argument for easier consumption.

```clojure
(intent "Convey the relationship between front- and backend.")

(name Frontend)
(name Backend)

(sense feeds_into "{Producer} feeds into {Consumer}")

(detail
  (intent "Explain in more detail the interaction between front- and backend.")

  ;; Names and senses declared here are local to this scope and not visible outside.
  (name AST)
  (sense produces "{Component} produces {Representation}")
  (sense consumes "{Component} accepts {Representation} as input")

  (claim (produces Frontend AST) (witness ...))
  (claim (consumes Backend AST) (witness ...))

  ;; Claims made about names and senses from the outer scope become visible there.
  (claim (feeds_into Frontend Backend)
    (and
     (produces Frontend AST)
     (consumes Backend AST))))
```

A [full grammar](docs/brief/grammar.md) is available in the docs.


## Verification with `check`

```
autoground check examples/search-calls-oracle.brief
autoground check examples/sqrt2.brief
```

Briefs compile to a set of inter-dependent claims, which `check` then validates starting
from the referenced witnesses. Claims are in natural language, and so can be imprecise,
handwavy, or subjective. We therefore cannot rely on traditional formal proof kernels.
Instead, we use a [system one model](https://docs.typesafe.ai/concepts/system-one) as an
oracle, a sort of neural proof kernel.

At every step in the proof the oracle is provided with the relevant hypotheses and is asked
to judge the proposed conclusion as "supported", "contradicted", or "neutral". This starts
with the files referenced by witnesses. Every `claim` is a deductive step. Only logical
operators are resolved determinisitically, without involving the oracle. Each unique claim
is only proved once, no matter how often it is referred to.

Exit status is 0 when every top-level claim is discharged, 1 otherwise, and 3 on a compile,
IO, or oracle failure. Each check writes a detailed trace of all proof steps along with the
requests sent to the oracle.
