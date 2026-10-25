# Brief grammar

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
sentence  ::= "(" reading arg* ")"
reading   ::= ident | string
arg       ::= ident | string
ident     ::= [qualifier "/"] ident-name
path      ::= string | "$"
```

`ident-name` is an unqualified identifier. A comma is whitespace. `;;` comments through the
end of the line.

A scope is the file, or one `detail`. It has at most one `intent`, and the string is not
empty. A second intent is a compile error. A `detail` contains a claim or an assumption,
walking into nested details. `ns` stays at the top of the file.

`witness` is only the support of a `claim`. A path is relative to the brief. The symbol
`$` is the file a check was given. One sentence is
defined once in the file, by a `claim` or an `assume`. A claim whose support reaches its own
sentence is rejected. Names and senses declared inside a `detail` stay there. Names and
senses declared outside gain the sentences validated inside.

```brief
(name Frontend)
(name Backend)
(sense depends_on "{X} depends on {Y}")
(and (depends_on Frontend Backend) (not (depends_on Backend Frontend)))
```

```brief
(claim ("alpha is the first letter.")
  (witness "note.txt"))
```

```brief
(assume ("the file is small"))
(claim ("the brief may treat the file as small")
  ("the file is small"))
```

```brief
(name Concept)
(sense input "{X} is an input into {Y}")
(detail
  (intent "inputs")
  (name LogicBlock)
  (claim (input Concept LogicBlock)
    ("Concept is closed over")))
```

```brief
(ns example.grammar)
```
