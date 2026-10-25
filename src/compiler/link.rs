use std::collections::{HashMap, HashSet};

use crate::syntax::{Program, Refer, Sense};

use super::scan::Loaded;
use super::{compile_with, CompileError, Compiled, Imports, Opened};

struct Item<'a> {
    path: &'a str,
    program: &'a Program,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Name,
    Sense,
}

/// Resolve `entry`'s requires against `files` and compile `entry`.
///
/// `files` is every namespaced model the scan found. The declared namespace
/// is the key. `entry_path` is that same path when the entry file was part of
/// the scan, so it is indexed once. Required namespaces are compiled, and
/// their claims stay out of the returned model.
pub fn link(
    entry: &Program,
    entry_path: Option<&str>,
    files: &[Loaded],
) -> Result<Compiled, CompileError> {
    Ok(link_parts(entry, entry_path, files)?.0)
}

/// [`link`], plus the vocabulary the entry can see through its requires.
pub fn link_parts(
    entry: &Program,
    entry_path: Option<&str>,
    files: &[Loaded],
) -> Result<(Compiled, super::Vocabulary), CompileError> {
    let index = build_index(entry, entry_path, files)?;
    let order = dependency_order(entry, &index)?;
    for ns in &order {
        let program = index[ns].program;
        let imports = environment(program, &index)?;
        compile_with(program, &imports).map_err(|err| CompileError {
            message: format!("in `{ns}`: {}", err.message),
        })?;
    }
    let imports = environment(entry, &index)?;
    let vocab = super::Vocabulary::from_imports(&imports);
    let compiled = compile_with(entry, &imports)?;
    Ok((compiled, vocab))
}

fn build_index<'a>(
    entry: &'a Program,
    entry_path: Option<&'a str>,
    files: &'a [Loaded],
) -> Result<HashMap<String, Item<'a>>, CompileError> {
    let mut index = HashMap::new();
    for file in files {
        if entry_path == Some(file.path.as_str()) {
            continue;
        }
        if file.program.namespace.is_some() {
            insert(&mut index, &file.path, &file.program)?;
        }
    }
    if entry.namespace.is_some() {
        insert(&mut index, entry_path.unwrap_or("<input>"), entry)?;
    }
    Ok(index)
}

fn insert<'a>(
    index: &mut HashMap<String, Item<'a>>,
    path: &'a str,
    program: &'a Program,
) -> Result<(), CompileError> {
    let name = &program.namespace.as_ref().unwrap().name;
    if let Some(prev) = index.get(name.as_str()) {
        let (first, second) = if prev.path <= path {
            (prev.path, path)
        } else {
            (path, prev.path)
        };
        return Err(CompileError {
            message: format!("namespace `{name}` is declared in `{first}` and `{second}`"),
        });
    }
    index.insert(name.clone(), Item { path, program });
    Ok(())
}

fn dependency_order(
    entry: &Program,
    index: &HashMap<String, Item<'_>>,
) -> Result<Vec<String>, CompileError> {
    let Some(ns) = &entry.namespace else {
        return Ok(Vec::new());
    };
    let mut seen = HashSet::new();
    let mut order = Vec::new();
    visit(&ns.name, &ns.name, index, &mut seen, &mut order)?;
    Ok(order)
}

fn visit(
    ns: &str,
    entry_ns: &str,
    index: &HashMap<String, Item<'_>>,
    seen: &mut HashSet<String>,
    order: &mut Vec<String>,
) -> Result<(), CompileError> {
    if !seen.insert(ns.to_string()) {
        return Ok(());
    }
    let program = index[ns].program;
    let requires = &program.namespace.as_ref().unwrap().requires;
    let mut local = HashSet::new();
    for require in requires {
        if require.namespace == ns {
            return Err(CompileError {
                message: format!("`{ns}` requires itself"),
            });
        }
        if !local.insert(require.namespace.clone()) {
            return Err(CompileError {
                message: format!("`{}` is required twice", require.namespace),
            });
        }
        if !index.contains_key(&require.namespace) {
            return Err(CompileError {
                message: format!("namespace `{}` is not declared", require.namespace),
            });
        }
        visit(&require.namespace, entry_ns, index, seen, order)?;
    }
    if ns != entry_ns {
        order.push(ns.to_string());
    }
    Ok(())
}

fn environment(
    program: &Program,
    index: &HashMap<String, Item<'_>>,
) -> Result<Imports, CompileError> {
    let here = program
        .namespace
        .as_ref()
        .map(|ns| ns.name.as_str())
        .unwrap_or("");
    let requires = program
        .namespace
        .as_ref()
        .map(|ns| ns.requires.as_slice())
        .unwrap_or(&[]);
    let names_here: HashSet<String> = program.names().map(|name| name.to_string()).collect();
    let senses_here: HashMap<String, Sense> = program
        .senses()
        .map(|sense| (sense.name.clone(), sense.clone()))
        .collect();

    let mut imported_names: HashMap<String, String> = HashMap::new();
    let mut imported_senses: HashMap<String, (String, Sense)> = HashMap::new();
    let mut aliases: HashMap<String, String> = HashMap::new();
    let mut opened: HashMap<String, Opened> = HashMap::new();
    let mut once = HashSet::new();

    for require in requires {
        if require.namespace == here {
            return Err(CompileError {
                message: format!("`{here}` requires itself"),
            });
        }
        if !once.insert(require.namespace.clone()) {
            return Err(CompileError {
                message: format!("`{}` is required twice", require.namespace),
            });
        }
        let Some(item) = index.get(&require.namespace) else {
            return Err(CompileError {
                message: format!("namespace `{}` is not declared", require.namespace),
            });
        };
        if let Some(alias) = &require.alias {
            if let Some(prev) = aliases.get(alias) {
                return Err(CompileError {
                    message: format!(
                        "`{alias}` aliases both `{prev}` and `{}`",
                        require.namespace
                    ),
                });
            }
            aliases.insert(alias.clone(), require.namespace.clone());
        }
        let declared_names: HashSet<String> =
            item.program.names().map(|name| name.to_string()).collect();
        let declared_senses: HashMap<String, Sense> = item
            .program
            .senses()
            .map(|sense| (sense.name.clone(), sense.clone()))
            .collect();
        let spellings: Vec<String> = match &require.refer {
            Refer::Nothing => Vec::new(),
            Refer::All => {
                let mut all: Vec<_> = declared_names.iter().cloned().collect();
                all.extend(declared_senses.keys().cloned());
                all
            }
            Refer::Only(list) => {
                for spelling in list {
                    if !declared_names.contains(spelling) && !declared_senses.contains_key(spelling)
                    {
                        return Err(CompileError {
                            message: format!(
                                "`{spelling}` is not declared in `{}`",
                                require.namespace
                            ),
                        });
                    }
                }
                list.clone()
            }
        };
        for spelling in &spellings {
            let as_name = declared_names.contains(spelling);
            let as_sense = declared_senses.contains_key(spelling);
            if as_name && as_sense {
                return Err(CompileError {
                    message: format!("`{spelling}` is declared as both a name and a sense"),
                });
            }
            let binding = if as_name {
                Binding::Name
            } else {
                Binding::Sense(declared_senses[spelling].clone())
            };
            introduce(
                &mut Table {
                    here,
                    names_here: &names_here,
                    senses_here: &senses_here,
                    imported_names: &mut imported_names,
                    imported_senses: &mut imported_senses,
                },
                spelling,
                binding,
                &require.namespace,
            )?;
        }
        opened.insert(
            require.namespace.clone(),
            Opened {
                names: declared_names,
                senses: declared_senses,
            },
        );
    }

    let mut qualifiers = HashMap::new();
    for ns in opened.keys() {
        qualifiers.insert(ns.clone(), ns.clone());
    }
    for (alias, ns) in &aliases {
        qualifiers.insert(alias.clone(), ns.clone());
    }
    let senses = imported_senses
        .into_iter()
        .map(|(spelling, (_, sense))| (spelling, sense))
        .collect();
    Ok(Imports {
        names: imported_names.into_keys().collect(),
        senses,
        qualifiers,
        opened,
    })
}

enum Binding {
    Name,
    Sense(Sense),
}

struct Table<'a> {
    here: &'a str,
    names_here: &'a HashSet<String>,
    senses_here: &'a HashMap<String, Sense>,
    imported_names: &'a mut HashMap<String, String>,
    imported_senses: &'a mut HashMap<String, (String, Sense)>,
}

fn introduce(
    table: &mut Table<'_>,
    spelling: &str,
    binding: Binding,
    owner: &str,
) -> Result<(), CompileError> {
    let kind = match &binding {
        Binding::Name => Kind::Name,
        Binding::Sense(_) => Kind::Sense,
    };
    if table.names_here.contains(spelling) {
        return Err(clash(spelling, Kind::Name, table.here, kind, owner));
    }
    if table.senses_here.contains_key(spelling) {
        return Err(clash(spelling, Kind::Sense, table.here, kind, owner));
    }
    if let Some(prev) = table.imported_names.get(spelling) {
        return Err(clash(spelling, Kind::Name, prev, kind, owner));
    }
    if let Some((prev, _)) = table.imported_senses.get(spelling) {
        return Err(clash(spelling, Kind::Sense, prev, kind, owner));
    }
    match binding {
        Binding::Name => {
            table
                .imported_names
                .insert(spelling.to_string(), owner.to_string());
        }
        Binding::Sense(sense) => {
            table
                .imported_senses
                .insert(spelling.to_string(), (owner.to_string(), sense));
        }
    }
    Ok(())
}

fn clash(
    spelling: &str,
    left_kind: Kind,
    left: &str,
    right_kind: Kind,
    right: &str,
) -> CompileError {
    let message = if left_kind == right_kind {
        format!("`{spelling}` is declared in `{left}` and `{right}`")
    } else {
        format!(
            "`{spelling}` is a {} in `{left}` and a {} in `{right}`",
            kind_word(left_kind),
            kind_word(right_kind)
        )
    };
    CompileError { message }
}

fn kind_word(kind: Kind) -> &'static str {
    match kind {
        Kind::Name => "name",
        Kind::Sense => "sense",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse;
    use crate::Proposition;

    fn loaded(path: &str, src: &str) -> Loaded {
        Loaded {
            path: path.to_string(),
            program: parse(src).unwrap(),
        }
    }

    fn linked(entry: &str, files: &[Loaded]) -> Result<Compiled, CompileError> {
        let program = parse(entry).unwrap();
        link(&program, Some("<input>"), files)
    }

    const PRODUCT: &str = r#"
(ns com.acme.product)
(name Frontend)
(sense part_of "{X} is part of {Y}")
(assume (part_of Frontend Frontend))
"#;

    #[test]
    fn alias_reaches_a_name_and_a_sense_only_when_qualified() {
        let files = vec![loaded("weird/place/product.brief", PRODUCT)];
        let compiled = linked(
            r#"
(ns com.acme.client
  (:require [com.acme.product :as product]))
(name product)
(name Backend)
(assume (product/part_of product/Frontend Backend))
"#,
            &files,
        )
        .unwrap();
        assert_eq!(
            compiled.claims,
            vec![crate::syntax::Proposition::Assume(
                "Frontend is part of Backend".into()
            )]
        );
        let unqualified = linked(
            r#"
(ns com.acme.client
  (:require [com.acme.product :as product]))
(name Backend)
(assume (part_of Frontend Backend))
"#,
            &files,
        )
        .unwrap_err();
        assert!(
            unqualified.message.contains("not a declared sense")
                || unqualified.message.contains("not a declared name"),
            "{unqualified}"
        );
    }

    #[test]
    fn refer_and_refer_all_import_names_and_senses() {
        let files = vec![loaded("a.brief", PRODUCT)];
        let referred = linked(
            r#"
(ns com.acme.client
  (:require [com.acme.product :refer [Frontend part_of]]))
(name Backend)
(assume (part_of Frontend Backend))
"#,
            &files,
        )
        .unwrap();
        let all = linked(
            r#"
(ns com.acme.client
  (:require [com.acme.product :refer :all]))
(name Backend)
(assume (part_of Frontend Backend))
"#,
            &files,
        )
        .unwrap();
        assert_eq!(referred.claims, all.claims);
        assert_eq!(
            referred.claims,
            vec![crate::syntax::Proposition::Assume(
                "Frontend is part of Backend".into()
            )]
        );
        let bare = linked(
            r#"
(ns com.acme.client
  (:require [com.acme.product]))
(assume (com.acme.product/part_of com.acme.product/Frontend com.acme.product/Frontend))
"#,
            &files,
        )
        .unwrap();
        assert_eq!(
            bare.claims,
            vec![crate::syntax::Proposition::Assume(
                "Frontend is part of Frontend".into()
            )]
        );
    }

    #[test]
    fn refer_clash_missing_symbol_missing_namespace_and_self_require() {
        let files = vec![
            loaded("a.brief", PRODUCT),
            loaded(
                "b.brief",
                r#"
(ns com.acme.other)
(name Frontend)
(sense calls "{X} calls {Y}")
"#,
            ),
        ];
        let clash = linked(
            r#"
(ns com.acme.client
  (:require [com.acme.product :refer [Frontend]]
            [com.acme.other :refer [Frontend]]))
"#,
            &files,
        )
        .unwrap_err();
        assert!(
            clash.message.contains("com.acme.product") && clash.message.contains("com.acme.other"),
            "{clash}"
        );

        let local = linked(
            r#"
(ns com.acme.client
  (:require [com.acme.product :refer :all]))
(name Frontend)
"#,
            &files,
        )
        .unwrap_err();
        assert!(
            local.message.contains("com.acme.client") && local.message.contains("com.acme.product"),
            "{local}"
        );

        let missing_symbol = linked(
            "(ns com.acme.client (:require [com.acme.product :refer [Nope]]))",
            &files,
        )
        .unwrap_err();
        assert!(
            missing_symbol.message.contains("Nope")
                && missing_symbol.message.contains("com.acme.product"),
            "{missing_symbol}"
        );

        let missing_ns = linked(
            "(ns com.acme.client (:require [com.acme.missing :as missing]))",
            &files,
        )
        .unwrap_err();
        assert!(
            missing_ns.message.contains("not declared") && !missing_ns.message.contains("com/acme"),
            "{missing_ns}"
        );

        let itself = linked(
            "(ns com.acme.client (:require [com.acme.client :as self]))",
            &files,
        )
        .unwrap_err();
        assert!(itself.message.contains("requires itself"), "{itself}");

        let twice = linked(
            r#"
(ns com.acme.client
  (:require [com.acme.product :as product]
            [com.acme.product :refer :all]))
"#,
            &files,
        )
        .unwrap_err();
        assert!(twice.message.contains("required twice"), "{twice}");
    }

    #[test]
    fn a_duplicate_namespace_names_both_paths() {
        let files = vec![
            loaded("notes/readme.md", "(ns com.acme.product)\n"),
            loaded("other/place.brief", "(ns com.acme.product)\n"),
        ];
        let err = link(&files[0].program, Some("notes/readme.md"), &files).unwrap_err();
        assert!(
            err.message.contains("notes/readme.md")
                && err.message.contains("other/place.brief")
                && err.message.contains("com.acme.product"),
            "{err}"
        );
    }

    #[test]
    fn a_bad_dependency_claim_fails_and_its_sentences_stay_out() {
        let broken = loaded(
            "product.brief",
            r#"
(ns com.acme.product)
(name Frontend)
(sense part_of "{X} is part of {Y}")
(assume (part_of Frontend Missing))
"#,
        );
        let err = linked(
            r#"
(ns com.acme.client
  (:require [com.acme.product :refer [Frontend part_of]]))
(name Backend)
(assume (part_of Frontend Backend))
"#,
            &[broken],
        )
        .unwrap_err();
        assert!(
            err.message.contains("com.acme.product") && err.message.contains("Missing"),
            "{err}"
        );

        let sound = loaded("product.brief", PRODUCT);
        let compiled = linked(
            r#"
(ns com.acme.client
  (:require [com.acme.product :refer [Frontend part_of]]))
(name Backend)
(assume (part_of Frontend Backend))
"#,
            &[sound],
        )
        .unwrap();
        assert_eq!(
            compiled.claims,
            vec![Proposition::Assume("Frontend is part of Backend".into())]
        );
    }

    #[test]
    fn cycles_are_allowed() {
        let files = vec![
            loaded(
                "a.brief",
                r#"
(ns com.acme.a
  (:require [com.acme.b :refer [Backend]]))
(name Frontend)
(sense part_of "{X} is part of {Y}")
(assume (part_of Frontend Backend))
"#,
            ),
            loaded(
                "b.brief",
                r#"
(ns com.acme.b
  (:require [com.acme.a :refer [Frontend part_of]]))
(name Backend)
(assume (part_of Frontend Backend))
"#,
            ),
        ];
        let compiled = link(&files[0].program, Some("a.brief"), &files).unwrap();
        assert_eq!(
            compiled.claims,
            vec![crate::syntax::Proposition::Assume(
                "Frontend is part of Backend".into()
            )]
        );
    }
}
