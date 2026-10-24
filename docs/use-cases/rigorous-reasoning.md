# Construct a rigorous argument with `/proof`

Proofs, formal or informal, are tools of discovery and alignment. In an attempt to prove our
conclusions to a pedantic judge, humans and agents uncover hidden assumptions and are forced
to clarify ambiguous concepts. This builds trust.

A judge just needs to be reasonable and consistent: the [Lean
compiler](https://lean-lang.org), reviewer #2 in an academic journal, or a child asking
"why?" can all play the role in a useful way. autoground currently uses the [jev system one
model](https://docs.typesafe.ai/concepts/system-one) as its judge.

The [`/proof` skill](../../skills/proof/SKILL.md) instructs an agent to record reasoning
steps (yours or theirs!) in a `*.brief` (or `*.md` with `brief` fences), and then iterate
until `autoground check` is satisfied. Typically each iteration involves the following
steps:

1. Clarifying assumptions.

```clojure
(sense even-square "The square of an even whole number is even.")
(sense odd-square "The square of an odd whole number is odd.")
(sense even-or-odd "Every whole number is even or odd.")

(assume (even-square))
(assume (odd-square))
(assume (even-or-odd))
```

2. Breaking down complex claims into simpler ones.

```clojure
(sense theorem "The square root of two is not a ratio of two whole numbers.")

(claim (theorem)
  (and
    (has-lowest-terms)
    (lemma-15)))
```

3. Letting autoground judge.

```shell
autoground check sqrt2.brief
```

![sqrt2 demo](/docs/assets/vhs/sqrt2.webm)


## Example

The following is a brief attempting to prove the following theorem:

```clojure
(sense theorem "The square root of two is not a ratio of two whole numbers.")
```

It is written from the bottom-up, starting with assumptions that build up into lemmas.

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

