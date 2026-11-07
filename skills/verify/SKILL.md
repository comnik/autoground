---
name: verify
description: Turn the user's instructions into a rigorous validated argument.
argument-hint: "[instructions]"
disable-model-invocation: true
compatibility: Requires the autoground CLI on PATH.
license: MIT
---

The user is looking to either increase their trust in _your_ reasoning, or in _their own_. To increase trust, it is helpful to provide a step-by-step argument that is detailed enough for the `autoground` neural proof kernel to verify each step.

Without any more specific instructions, assume that you are being tasked to translate the user's thinking into a `brief` document (explained below). Otherwise, create the brief according to the user's instructions.

Use `autoground check` to validate your brief. Iterate until all claims are either validated, or the exercise has surfaced an inconsistency or missing assumption. Surface any such issues clearly to the user.

Once validated, present the brief back to the user in whatever format is most appropriate. Point them at the validated brief as a certificate that they can validate themselves. Always highlight assumptions and critical reasoning steps.

Remember, the goal is not to outsmart the proof system, but to get to a more precise understanding. If you're struggling to get the brief validated, work with the user to refine the assumptions and resolve contradictions.

## Guidelines for writing an effective brief

Refer to the `references/` directory for documentation of the brief language.

Pay special attention about the assumptions being made. Explicitly state each assumption you identify via an `assume` declaration. Assumptions are accepted as given, so the soundness of the entire argument hinges on their validity.

The human brain pays a tax for every name it has to hold in working memory. If a brief grows past ten names in the mainline argument, see if you can collapse some of the details into `(detail)` scopes.

Sometimes it is helpful to introduce a new name to use as an abbreviation, without affecting the semantic content of the brief. Refer to the documentation, which explains how to use nullary senses to capture such definitions.

If `autoground check` doesn't accept your brief, break down your claims into smaller, more atomic propositions combined via `and`, `or`, or `not`. Use the opportunity to double check your own understanding. If you notice that you have to do unreasonable things to "trick" the judge, just surface that back to the user. The goal is building real understanding, not passing the check at all cost.

Lastly, `autoground check` is not perfectly deterministic. A robust brief with the occasional near miss is expected and acceptable.

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