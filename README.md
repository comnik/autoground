# ⏚ autoground

autoground is an <ins>informal</ins> proof assistant.

Together with your favourite coding agent, autoground helps you to (re-)establish and
maintain an accurate mental model of a codebase that has drifted out from under you.

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


## Get started

With the autoground CLI and skills installed, follow one of our guided use cases.

**use [`/brief`](docs/use-cases/codebase-mental-model.md) to validate your mental model of a
codebase**

**use [`/proof`](docs/use-cases/rigorous-reasoning.md) to construct a rigorously reasoned
argument**

Use the provided skills as examples for writing your own! There are many other use cases to
discover.


## The `brief` language

Informal proof is a dynamic process where proof author and verifier iteratively align their
respective mental models and expose implicit assumptions. Aligning mental models requires
using the same name for things that relate in the same way. brief is a language with two
goals: 1) help establish shared terminology, and 2) support gradual decomposition of a
complicated argument into increasingly atomic claims.

> **[brief](https://www.merriam-webster.com/dictionary/brief)**<br/>
> *"a formal written presentation of an argument that sets forth the main points with
> supporting precedents and evidence"*

The language grammar and constructs are [explained in the docs](/docs/brief/language.md),
which are also included in the installed skills.

I suspect you'll rarely deal with the raw syntax without an agent intermediating, but take a
look at the autoground [architecture brief](/briefs/architecture/architecture.brief) if you
like. Most of the time, your agent should be handing you your preferred visual
representation to look at, see below.


## Verification with `check`

```
autoground check briefs/architecture/architecture.brief
```

Briefs compile to a set of inter-dependent claims, which `check` then validates starting
from the referenced witnesses.

At every step in the proof the oracle is provided with the collected support (witnesses or
other claims) and is asked to judge the proposed conclusion as "supported", "contradicted",
or "neutral". Every `claim` is a deductive step. Only logical operators are resolved
determinisitically, without involving the oracle. Each unique claim is only proved once, no
matter how often it is referred to.

Exit status is 0 when every top-level claim is discharged, 1 otherwise, and 3 on a compile,
IO, or oracle failure. Each check writes a detailed trace of all proof steps along with the
requests sent to the oracle.

To illustrate this flow, an agent created the below diagram based on the [architecture
brief](/briefs/architecture/architecture.brief). Each node and each arrow is backed by
witnesses from the autoground source code. All briefs are checked during pre-commit, so any
drift between implementation and diagram will be caught immediately.

![autoground architecture](/briefs/architecture/architecture.svg)