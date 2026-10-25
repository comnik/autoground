## The `brief` language

Informal proof is a dynamic process where proof author and verifier iteratively align their
respective mental models and expose implicit assumptions. Aligning mental models requires
using the same name for things that relate in the same way. brief is a language with two
goals: 1) help establish shared terminology, and 2) support gradual decomposition of a
complicated argument into increasingly atomic claims.

> **[brief](https://www.merriam-webster.com/dictionary/brief)**<br/>
> *"a formal written presentation of an argument that sets forth the main points with
> supporting precedents and evidence"*

## Grammar

A brief is a sequence of forms. `ns`, when present, is the first form and the only one.

```
program   ::= [ns] form*
ns        ::= "(" "ns" namespace [require] ")"
namespace ::= segments separated by ".", no empty segment, no "/"
require   ::= "(" ":require" libspec+ ")"
libspec   ::= "[" namespace [":as" alias] [":refer" refer] "]"
refer     ::= ":all" | "[" ident-name* "]"
form      ::= name | sense | intent | assume | detail | formula
name      ::= "(" "name" ident-name ")"
sense     ::= "(" "sense" ident-name string ")"
intent    ::= "(" "intent" string ")"
assume    ::= "(" "assume" sentence ")"
detail    ::= "(" "detail" form+ ")"
formula   ::= claim | and | or | not | sentence
claim     ::= "(" "claim" sentence support ")"
support   ::= witness | formula
witness   ::= "(" "witness" path+ ")"
and       ::= "(" "and" formula* ")"
or        ::= "(" "or" formula* ")"
not       ::= "(" "not" formula ")"
sentence  ::= string | "(" ident arg* ")" | "(" string arg+ ")"
arg       ::= ident | string
ident     ::= [qualifier "/"] ident-name
path      ::= string | "$"
```

`ident-name` is an unqualified identifier. A comma is whitespace. `;;` comments through the
end of the line.

A scope is the file, or one `detail`. It has at most one `intent`, and the string is not
empty. A second intent is a compile error. A `detail` contains a claim or an assumption,
walking into nested details. `ns` stays at the top of the file.

`witness` is only the support of a `claim`. A path is relative to the brief. The symbol `$`
is the file a check was given. One sentence is defined once in the file, by a `claim` or an
`assume`. A claim whose support reaches its own sentence is rejected. Names and senses
declared inside a `detail` stay there. Names and senses declared outside gain the sentences
validated inside.


## Constructs

### Namespaces

An optional namespace at the beginning of a brief disambiguates its contents from others.

```brief
(ns com.example)
```

### Names

Names identify concrete or abstract concepts. Components and data structures in the
codebase, mathematical objects, or things in the real world. They are the nodes in a mental
model.

```brief
(name Frontend)
(name Backend)
(name AST)
(name User)
(name Server)
```

### Senses

Senses define templates for attaching meaning to names and for establishing relationships
between them. Senses can be of any arity, including nullary.

```brief
(sense invariant "All traffic is encrypted")        ;; no free variables

(sense is_data "{Name} is a data structure")        ;; single free variable
(sense is_component "{X} is a component")           ;; single free variable
(sense returns "{Name} returns {Name}")             ;; two free variables

;; Note how the variables can be named as well, to disambiguate roles in a relationship.
(sense property "{object} has a {property}")
(sense borrowed_from "{borrower} borrowed from {lender}")

;; Ternary relationship with three different named roles.
(sense has_permission "{subject} is granted {permission} on {object}")
```

Applying names to a sense constructs a fully formed sentence:

```
   (returns Frontend AST)
=> ("{Name} returns {Name}" Frontend AST)
=> "Frontend returns AST"
```

### Claims

Senses instantiated with names can be used to make claims for autoground to judge.


```brief
(name Frontend)
(name Backend)
(sense depends_on "{X} depends on {Y}")
(and (depends_on Frontend Backend) (not (depends_on Backend Frontend)))
```

```brief
(claim "alpha is the first letter."
  (witness "note.txt"))
```

```brief
(assume "the file is small")
(claim "the brief may treat the file as small"
  "the file is small")
```

```brief
(name Concept)
(sense input "{X} is an input into {Y}")
(detail
  (intent "inputs")
  (name LogicBlock)
  (claim (input Concept LogicBlock)
    "Concept is closed over"))
```
