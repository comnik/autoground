---
name: proof
description: Construct a rigorous validated argument for the user.
argument-hint: "[instructions]"
disable-model-invocation: true
compatibility: Requires the autoground CLI on PATH.
license: MIT
---

Sometimes, the user is asking for a complex reasoned explanation. To increase trust, it is helpful to provide a step-by-step argument that is detailed enough for a neural proof kernel to verify each step.

Create a `brief` (explained below) based on the user's instructions. Pay special attention about the assumptions they want you to make, if any.

Explicitly state each assumption via `assume` declarations.

Use `autoground check` to validate your brief. Iterate until all claims are validated.

Once validated, surface the argument back to the user in whatever format is most appropriate. Point them at the validated `brief` as a certificate of your reasoning. Always highlight the assumptions and critical reasoning steps when you communicate to the user.

Remember, the goal is not to trick the proof system, but to construct a genuinely rigorous and validated argument. If you're struggling to get the argument validated, revisit your assumptions and reasoning steps to ensure they are sound.


## Creating a `brief`

Laying out an argument for the neural proof kernel requires using a lightly structured language called `brief`.

A brief consists of *names*, *senses*, *claims*, and *witnesses*:

```brief
;; A namespace globally disambiguates this brief.
(ns autoground)

;; Names identify concrete or abstract concepts. Components and data structures in the
;; codebase, mathematical objects, or things in the real world.
(name Frontend)
(name Backend)
(name Program)
(name AST)
(name Sweep)
(name Oracle)

;; Senses define templates for attaching meaning to names and for establishing
;; relationships between them. An application fills every hole, left to right.
(sense is_data "{Name} is a data structure")        ;; single free variable
(sense is_component "{X} is a component")           ;; single free variable
(sense returns "{Name} returns {Name}")             ;; two free variables
(sense calls "{caller} calls {callee}")

;; Role names on the holes disambiguate the arguments.
(sense property "{object} has a {property}")
(sense borrowed_from "{borrower} borrowed from {lender}")
(sense related "{X} is related to {Y}")
(sense depends_on "{X} depends on {Y}")

;; A statement on its own is unsupported. A claim defines the sentence.
(claim (is_component Frontend)
  (witness "src/frontend/mod.rs"))

(claim "Sweep asks the oracle whether a path matches the filter"
  (witness "src/sweep/mod.rs"))

;; A claim can be supported by another claim. Cite a sentence already claimed.
(sense lemma-1 "If a whole number is odd, then its square is not even.")
(sense lemma-2 "If the square of a whole number is even, then that whole number is not odd.")
(claim (lemma-1)
  (witness "src/lemma.rs"))
(claim (lemma-2)
  (lemma-1))

(claim (calls Sweep Oracle)
  (and
    "Sweep asks the oracle whether a path matches the filter"
    (claim "Sweep's oracle posts are the Jev HTTP call"
      (witness "src/sweep/mod.rs" "src/oracle/jev.rs"))))

;; A claim that cannot be validated from one witness takes `not`, `and`, or `or` as its support.
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

Applying a sense to names fills the template:

```
   (returns Frontend AST)
=> ("{Name} returns {Name}" Frontend AST)
=> "Frontend returns AST"
=> Frontend returns AST
```

`assume` takes a sentence as true, with no witness. `intent` says what understanding the brief is meant to convey.

```brief
(sense axiom "The square of an even whole number is even.")
(assume (axiom))

(intent "This brief is intended to convey the compilation pipeline.")
```

When you are making a rigorous argument in `brief` that is not grounded in any code or external evidence, you should use `assume` to introduce premises that you take as true for the sake of the argument.

The ordering of the top-level declarations is irrelevant.

Briefs are written to `*.brief` files. Wherever a `*.brief` file is accepted, a `.md` file is too: every fenced `brief` block is taken, in order, and the rest of the document is left out.

Briefs are not accepted as witnesses for other briefs, to make sure unvalidated claims are never treated as evidence.

A `detail` hides part of an argument. Names and senses declared inside it stay there. A claim inside it about a name from outside becomes visible outside. The claim still needs its own support.

```brief
(intent "Convey the relationship between front- and backend.")

(name Frontend)
(name Backend)

(sense feeds_into "{Producer} feeds into {Consumer}")

(detail
  (intent "Explain in more detail the interaction between front- and backend.")

  (name AST)
  (sense produces "{Component} produces {Representation}")
  (sense consumes "{Component} accepts {Representation} as input")

  (claim (produces Frontend AST) (witness "src/frontend/mod.rs"))
  (claim (consumes Backend AST) (witness "src/backend/mod.rs"))

  (claim (feeds_into Frontend Backend)
    (and
      (produces Frontend AST)
      (consumes Backend AST))))
```

## Example

```brief
;; The square root of two is irrational.
;; Axioms are named senses, taken as true with assume. Each lemma is a nullary
;; sense. The claim under it is the proof, and a later step cites the sense
;; instead of nesting the proof again.

(sense even-square "The square of an even whole number is even.")
(sense odd-square "The square of an odd whole number is odd.")
(sense even-or-odd "Every whole number is even or odd.")
(sense not-both "No whole number is both even and odd.")
(sense even-iff-twice "A whole number is even exactly when it is twice a whole number.")
(sense square-whole "The square of a whole number is a whole number.")
(sense square-of-twice "The square of twice a whole number is four times the square of that whole number.")
(sense four-equals-twice "If four times a whole number equals twice another whole number, then that other whole number is twice the first.")
(sense sqrt2-equation "Whenever a ratio of two whole numbers equals the square root of two, the square of the first equals twice the square of the second.")
(sense evens-share-two "Two even whole numbers share two as a common divisor.")
(sense lowest-terms "A fraction in lowest terms is a ratio of two whole numbers that share no common divisor greater than one.")
(sense has-lowest-terms "Every ratio of two whole numbers equals some fraction in lowest terms.")

(assume (even-square))
(assume (odd-square))
(assume (even-or-odd))
(assume (not-both))
(assume (even-iff-twice))
(assume (square-whole))
(assume (square-of-twice))
(assume (four-equals-twice))
(assume (sqrt2-equation))
(assume (evens-share-two))
(assume (lowest-terms))
(assume (has-lowest-terms))

(sense lemma-1 "If a whole number is odd, then its square is not even.")

(claim (lemma-1)
  (and
    (odd-square)
    (not-both)))

(sense lemma-2 "If the square of a whole number is even, then that whole number is not odd.")

(claim (lemma-2)
  (lemma-1))

(sense lemma-3 "A whole number whose square is even is itself even.")

(claim (lemma-3)
  (and
    (lemma-2)
    (even-or-odd)))

(sense lemma-4 "If a ratio of two whole numbers equals the square root of two, then the first whole number is even.")

(claim (lemma-4)
  (and
    (lemma-3)
    (sqrt2-equation)))

(sense lemma-5 "If a whole number is even, then it is twice a whole number.")

(claim (lemma-5)
  (even-iff-twice))

(sense lemma-6 "If a whole number is even, then its square is four times the square of some whole number.")

(claim (lemma-6)
  (and
    (lemma-5)
    (square-of-twice)))

(sense lemma-7 "If the first whole number is even, then its square is four times the square of some whole number.")

(claim (lemma-7)
  (lemma-6))

(sense lemma-8 "If a ratio of two whole numbers equals the square root of two and the first whole number is even, then the square of the first is four times the square of some whole number.")

(claim (lemma-8)
  (lemma-7))

(sense lemma-9 "If a ratio of two whole numbers equals the square root of two and the first whole number is even, then four times a whole number equals twice the square of the second.")

(claim (lemma-9)
  (and
    (lemma-8)
    (sqrt2-equation)))

(sense lemma-10 "If a ratio of two whole numbers equals the square root of two and the first whole number is even, then the square of the second is twice a whole number.")

(claim (lemma-10)
  (and
    (lemma-9)
    (four-equals-twice)
    (square-whole)))

(sense lemma-11 "A whole number that is twice a whole number is even.")

(claim (lemma-11)
  (even-iff-twice))

(sense lemma-12 "If a ratio of two whole numbers equals the square root of two and the first whole number is even, then the square of the second is even.")

(claim (lemma-12)
  (and
    (lemma-10)
    (lemma-11)))

(sense lemma-13 "If a ratio of two whole numbers equals the square root of two, then the first whole number is even and the square of the second is even.")

(claim (lemma-13)
  (and
    (lemma-4)
    (lemma-12)))

(sense lemma-14 "If a ratio of two whole numbers equals the square root of two, then both whole numbers are even.")

(claim (lemma-14)
  (and
    (lemma-13)
    (even-square)
    (odd-square)
    (even-or-odd)))

(sense lemma-15 "No fraction in lowest terms equals the square root of two.")

(claim (lemma-15)
  (and
    (lowest-terms)
    (evens-share-two)
    (lemma-14)))

(sense theorem "The square root of two is not a ratio of two whole numbers.")

(claim (theorem)
  (and
    (has-lowest-terms)
    (lemma-15)))
```