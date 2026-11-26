use std::collections::{HashMap, HashSet};
use std::path::{Component, Path};

use crate::syntax::{Program, Proposition, Refer, Sense, Sentence, Support};

use super::scan::Loaded;
use super::{
    compile_module, CompileError, Compiled, CompiledModule, ExportedCertify, ExportedGround,
    Imports, Opened, ResolvedDefine, SenseKey,
};

pub(crate) struct Item<'a> {
    pub(crate) path: &'a str,
    pub(crate) program: &'a Program,
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
    let (module, imports) = link_module(entry, entry_path, files)?;
    let vocab = super::Vocabulary::from_imports(&imports);
    Ok((module.compiled, vocab))
}

pub(crate) fn link_module(
    entry: &Program,
    entry_path: Option<&str>,
    files: &[Loaded],
) -> Result<(CompiledModule, Imports), CompileError> {
    let index = build_index(entry, entry_path, files)?;
    let order = dependency_order(entry, &index)?;
    let mut modules: HashMap<String, CompiledModule> = HashMap::new();
    for ns in &order {
        let item = &index[ns];
        let imports = environment(item.program, item.path, &index, &modules)?;
        let module = compile_module(item.program, &imports).map_err(|err| CompileError {
            message: format!("in `{ns}`: {}", err.message),
        })?;
        modules.insert(ns.clone(), module);
    }
    let target_path = entry_path.unwrap_or("");
    let imports = environment(entry, target_path, &index, &modules)?;
    let module = compile_module(entry, &imports)?;
    Ok((module, imports))
}

pub(crate) fn build_index<'a>(
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

pub(crate) fn dependency_order(
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

pub(crate) fn environment(
    program: &Program,
    target_path: &str,
    index: &HashMap<String, Item<'_>>,
    modules: &HashMap<String, CompiledModule>,
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
    let mut imported_defines: HashMap<SenseKey, (String, ResolvedDefine)> = HashMap::new();
    let mut imported_facts: HashMap<String, (String, Sentence)> = HashMap::new();
    let mut imported_certifies: HashMap<SenseKey, (String, ExportedCertify)> = HashMap::new();
    let mut imported_certified_facts: HashMap<String, (String, ExportedCertify)> = HashMap::new();
    let mut imported_grounds: HashMap<SenseKey, (String, ExportedGround)> = HashMap::new();
    let mut aliases: HashMap<String, String> = HashMap::new();
    let mut opened: HashMap<String, Opened> = HashMap::new();
    let mut once = HashSet::new();

    let mut reachable = Vec::new();
    let mut seen_deps = HashSet::new();
    for require in requires {
        collect_reachable(&require.namespace, index, &mut seen_deps, &mut reachable);
    }
    for ns in &reachable {
        if let Some(module) = modules.get(ns) {
            for (key, def) in &module.defines {
                if let Some((prev, _)) = imported_defines.get(key) {
                    return Err(CompileError {
                        message: format!(
                            "`{}` is defined in both `{prev}` and `{ns}`",
                            key.display(),
                        ),
                    });
                }
                imported_defines.insert(key.clone(), (ns.clone(), def.clone()));
            }
            for (text, sentence) in &module.facts {
                if let Some((prev, _)) = imported_facts.get(text) {
                    return Err(CompileError {
                        message: format!("`{text}` is defined in both `{prev}` and `{ns}`"),
                    });
                }
                imported_facts.insert(text.clone(), (ns.clone(), sentence.clone()));
            }
        }
    }

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
        if let Some(module) = modules.get(&require.namespace) {
            for (key, certify) in &module.certifies {
                if let Some((prev, _)) = imported_certifies.get(key) {
                    return Err(CompileError {
                        message: format!(
                            "`{}` is certified in both `{prev}` and `{}`",
                            key.display(),
                            require.namespace
                        ),
                    });
                }
                let rebased = rebase_certify(certify, item.path, target_path);
                imported_certifies.insert(key.clone(), (require.namespace.clone(), rebased));
            }
            for (fact, certify) in &module.certified_facts {
                if let Some((prev, _)) = imported_certified_facts.get(fact) {
                    return Err(CompileError {
                        message: format!(
                            "`{fact}` is certified in both `{prev}` and `{}`",
                            require.namespace
                        ),
                    });
                }
                let rebased = rebase_certify(certify, item.path, target_path);
                imported_certified_facts.insert(fact.clone(), (require.namespace.clone(), rebased));
            }
            for (key, ground) in &module.grounds {
                if let Some((prev, _)) = imported_grounds.get(key) {
                    return Err(CompileError {
                        message: format!(
                            "`{}` is grounded in both `{prev}` and `{}`",
                            key.display(),
                            require.namespace
                        ),
                    });
                }
                let rebased = rebase_ground(ground, item.path, target_path);
                imported_grounds.insert(key.clone(), (require.namespace.clone(), rebased));
            }
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
    let mut owners = HashMap::new();
    let senses = imported_senses
        .into_iter()
        .map(|(spelling, (owner, sense))| {
            owners.insert(spelling.clone(), owner);
            (spelling, sense)
        })
        .collect();
    Ok(Imports {
        names: imported_names.into_keys().collect(),
        senses,
        qualifiers,
        opened,
        owners,
        defines: imported_defines
            .into_iter()
            .map(|(k, (_, v))| (k, v))
            .collect(),
        facts: imported_facts
            .into_iter()
            .map(|(k, (_, v))| (k, v))
            .collect(),
        certifies: imported_certifies
            .into_iter()
            .map(|(k, (_, v))| (k, v))
            .collect(),
        certified_facts: imported_certified_facts
            .into_iter()
            .map(|(k, (_, v))| (k, v))
            .collect(),
        grounds: imported_grounds
            .into_iter()
            .map(|(k, (_, v))| (k, v))
            .collect(),
    })
}

fn collect_reachable(
    ns: &str,
    index: &HashMap<String, Item<'_>>,
    seen: &mut HashSet<String>,
    out: &mut Vec<String>,
) {
    if !seen.insert(ns.to_string()) {
        return;
    }
    if let Some(item) = index.get(ns) {
        if let Some(decl) = &item.program.namespace {
            for req in &decl.requires {
                collect_reachable(&req.namespace, index, seen, out);
            }
        }
        out.push(ns.to_string());
    }
}

fn rebase_certify(
    certify: &ExportedCertify,
    from_file: &str,
    to_file: &str,
) -> ExportedCertify {
    ExportedCertify {
        cert_id: certify.cert_id.clone(),
        certificate: certify
            .certificate
            .iter()
            .map(|prop| rebase_prop(prop, from_file, to_file))
            .collect(),
        assumptions: certify.assumptions.clone(),
    }
}

fn rebase_ground(ground: &ExportedGround, from_file: &str, to_file: &str) -> ExportedGround {
    ExportedGround {
        cert_id: ground.cert_id.clone(),
        witness: ground
            .witness
            .iter()
            .map(|p| rebase_path(p, from_file, to_file))
            .collect(),
        certificate: ground
            .certificate
            .iter()
            .map(|prop| rebase_prop(prop, from_file, to_file))
            .collect(),
        assumptions: ground.assumptions.clone(),
    }
}

fn rebase_prop(prop: &Proposition, from_file: &str, to_file: &str) -> Proposition {
    match prop {
        Proposition::And(xs) => Proposition::And(
            xs.iter()
                .map(|x| rebase_prop(x, from_file, to_file))
                .collect(),
        ),
        Proposition::Or(xs) => Proposition::Or(
            xs.iter()
                .map(|x| rebase_prop(x, from_file, to_file))
                .collect(),
        ),
        Proposition::Not(inner) => {
            Proposition::Not(Box::new(rebase_prop(inner, from_file, to_file)))
        }
        Proposition::Sentence(s) => Proposition::Sentence(s.clone()),
        Proposition::Assume(s) => Proposition::Assume(s.clone()),
        Proposition::Defined { sentence, body } => Proposition::Defined {
            sentence: sentence.clone(),
            body: Box::new(rebase_prop(body, from_file, to_file)),
        },
        Proposition::Claim { sentence, support } => Proposition::Claim {
            sentence: sentence.clone(),
            support: rebase_support(support, from_file, to_file),
        },
        Proposition::Cert {
            cert_id,
            label,
            targets,
            support,
            requires,
            imported,
        } => Proposition::Cert {
            cert_id: cert_id.clone(),
            label: label.clone(),
            targets: targets.clone(),
            support: rebase_support(support, from_file, to_file),
            requires: requires.clone(),
            imported: imported
                .iter()
                .map(|p| rebase_prop(p, from_file, to_file))
                .collect(),
        },
        Proposition::Grounded {
            sentence,
            cert_id,
            certificate,
            witness,
        } => Proposition::Grounded {
            sentence: sentence.clone(),
            cert_id: cert_id.clone(),
            certificate: certificate
                .iter()
                .map(|p| rebase_prop(p, from_file, to_file))
                .collect(),
            witness: witness
                .iter()
                .map(|p| rebase_path(p, from_file, to_file))
                .collect(),
        },
        Proposition::Scope { body } => Proposition::Scope {
            body: body
                .iter()
                .map(|x| rebase_prop(x, from_file, to_file))
                .collect(),
        },
    }
}

fn rebase_support(support: &Support, from_file: &str, to_file: &str) -> Support {
    match support {
        Support::Witness { paths } => Support::Witness {
            paths: paths
                .iter()
                .map(|p| rebase_path(p, from_file, to_file))
                .collect(),
        },
        Support::Sentence(inner) => {
            Support::Sentence(Box::new(rebase_prop(inner, from_file, to_file)))
        }
    }
}

fn rebase_path(witness: &str, from_file: &str, to_file: &str) -> String {
    if witness == "$" {
        return "$".to_string();
    }
    let from_dir = dir_components(from_file);
    let to_dir = dir_components(to_file);
    if from_dir == to_dir {
        return witness.to_string();
    }
    let mut target = from_dir;
    push_normalized(&mut target, witness);

    let mut common = 0;
    while common < to_dir.len() && common < target.len() && to_dir[common] == target[common] {
        common += 1;
    }
    let mut rel = Vec::new();
    for _ in common..to_dir.len() {
        rel.push("..".to_string());
    }
    rel.extend_from_slice(&target[common..]);
    if rel.is_empty() {
        ".".to_string()
    } else {
        rel.join("/")
    }
}

fn dir_components(file: &str) -> Vec<String> {
    if file.is_empty() || file == "<input>" {
        return Vec::new();
    }
    let Some(parent) = Path::new(file).parent() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    push_normalized(&mut out, &parent.to_string_lossy());
    out
}

fn push_normalized(out: &mut Vec<String>, path: &str) {
    for comp in Path::new(path).components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if out.last().is_some_and(|last| last != "..") {
                    out.pop();
                } else {
                    out.push("..".to_string());
                }
            }
            Component::Normal(seg) => out.push(seg.to_string_lossy().into_owned()),
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
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

    #[test]
    fn a_qualified_term_verbalizes_as_its_bare_name() {
        let files = vec![loaded(
            "lib.brief",
            r#"
(ns lib)
(sense LogicOutputTables "the tables named in output_table_names")
(sense creates "{a} creates {b}")
"#,
        )];
        let compiled = linked(
            r#"
(ns app (:require [lib :as lib]))
(name R)
(assume (lib/creates R (lib/LogicOutputTables)))
"#,
            &files,
        )
        .unwrap();
        let Proposition::Assume(sentence) = &compiled.claims[0] else {
            panic!("assume");
        };
        assert_eq!(sentence.text, "R creates LogicOutputTables");
        assert_eq!(
            sentence.grounded,
            "R creates the tables named in output_table_names"
        );
    }

    #[test]
    fn a_referred_term_and_its_qualified_use_are_one_sense() {
        let files = vec![loaded(
            "lib.brief",
            r#"
(ns lib)
(sense LogicOutputTables "the tables")
"#,
        )];
        let err = linked(
            r#"
(ns app (:require [lib :as lib :refer [LogicOutputTables]]))
(name R)
(sense creates "{a} creates {b}")
(assume (creates R (LogicOutputTables)))
(assume (lib/LogicOutputTables))
"#,
            &files,
        )
        .unwrap_err();
        assert!(
            err.message
                .contains("`LogicOutputTables` is used as a term")
                && err.message.contains("as a sentence"),
            "{err}"
        );
    }

    #[test]
    fn ground_links_across_namespaces_rebasing_witnesses_and_assumptions() {
        let files = vec![
            loaded(
                "ontology.brief",
                r#"
(ns acme.ontology)
(sense customer "{c} is a customer")
(sense high_value "{c} is a high-value customer")
(define (high_value c)
  (and
    (customer c)
    ("{c} has lifetime sales > $25 million" c)))
"#,
            ),
            loaded(
                "generators/hv/certificate.brief",
                r#"
(ns generators.hv
  (:require [acme.ontology :refer [high_value]]))
(claim "query.sql selects high-value customers" (witness "query.sql"))
(assume "out.csv is up to date")
(ground (high_value c)
  (witness "out.csv")
  (and "query.sql selects high-value customers" "out.csv is up to date"))
"#,
            ),
        ];
        let entry = parse(
            r#"
(ns acme.analysis
  (:require
    [acme.ontology :refer [high_value]]
    [generators.hv]))
(name Acme)
(and (high_value Acme))
"#,
        )
        .unwrap();
        let compiled = link(&entry, Some("analysis/decision.brief"), &files).unwrap();
        assert_eq!(compiled.assumptions, vec!["out.csv is up to date"]);
        let Proposition::And(xs) = &compiled.claims[0] else {
            panic!("and");
        };
        let Proposition::Grounded {
            sentence,
            witness,
            certificate,
            ..
        } = &xs[0]
        else {
            panic!("expected Grounded");
        };
        assert_eq!(sentence.text, "Acme is a high-value customer");
        assert_eq!(witness, &vec!["../generators/hv/out.csv".to_string()]);
        assert_eq!(certificate.len(), 3);
        let Proposition::Claim {
            support: Support::Witness { paths },
            ..
        } = &certificate[0]
        else {
            panic!("expected lifted query claim");
        };
        assert_eq!(paths, &vec!["../generators/hv/query.sql".to_string()]);
    }

    #[test]
    fn bodyless_define_exports_across_namespaces_and_can_be_claimed() {
        let files = vec![loaded(
            "ontology.brief",
            r#"
(ns acme.ontology)
(name CorporateAction)
(name Merger)
(sense is_a "{X} is a sub-type of {Y}")
(define (is_a Merger CorporateAction))
"#,
        )];
        let consumer = parse(
            r#"
(ns acme.consumer (:require [acme.ontology :refer :all]))
(and (is_a Merger CorporateAction))
"#,
        )
        .unwrap();
        let compiled = link(&consumer, None, &files).unwrap();
        assert!(compiled.assumptions.is_empty());
        assert_eq!(
            compiled.claims,
            vec![Proposition::And(vec![Proposition::Defined {
                sentence: "Merger is a sub-type of CorporateAction".into(),
                body: Box::new(Proposition::And(Vec::new())),
            }])]
        );

        let verifier = parse(
            r#"
(ns acme.verify (:require [acme.ontology :refer :all]))
(claim (is_a Merger CorporateAction) (witness "types.rs"))
(and (is_a Merger CorporateAction))
"#,
        )
        .unwrap();
        let verified = link(&verifier, None, &files).unwrap();
        assert_eq!(
            verified.claims[1],
            Proposition::And(vec![Proposition::Sentence(
                "Merger is a sub-type of CorporateAction".into()
            )])
        );
    }

    #[test]
    fn certify_separates_implementation_adherence_from_data_grounding() {
        let files = vec![
            loaded(
                "ontology.brief",
                r#"
(ns acme.ibor)
(name Transaction)
(name Trade)
(sense is_a "{Sub} is a kind of {Super}")
(sense stacked "{Transaction} has {Count} cancels stacked on it")
(sense is_odd "{Count} is odd")
(sense canceled_by "{Transaction} is canceled by {Cancel}")
(sense canceled "{Transaction} is canceled")
(sense erased "{Transaction} is erased by {Cancel}")
(define (is_a Trade Transaction))
(define (canceled t)
  (exists [n] (and (stacked t n) (is_odd n))))
(define (erased t c)
  (and (canceled t) (canceled_by t c)))
"#,
            ),
            loaded(
                "impl/cert.brief",
                r#"
(ns acme.ibor.impl (:require [acme.ibor :refer :all]))
(certify (is_a Trade Transaction) (witness "ontology.py"))
(certify (canceled t) (witness "cancel_chains.py"))
(certify (erased t c) (witness "ibor.py"))
"#,
            ),
            loaded(
                "run/ground.brief",
                r#"
(ns acme.ibor.run
  (:require
    [acme.ibor :refer :all]
    [acme.ibor.impl]))
(assume "erased.csv came from ibor.py")
(ground (erased t c)
  (witness "erased.csv")
  (and
    (claim "ibor.py writes erased transactions to erased.csv" (witness "../impl/ibor.py"))
    "erased.csv came from ibor.py"))
"#,
            ),
        ];

        // Layer 2: certify emits Claim for fact, iff Certs for rules, and links erased -> canceled.
        let impl_compiled = link(&files[1].program, Some(files[1].path.as_str()), &files).unwrap();
        assert!(matches!(
            &impl_compiled.claims[0],
            Proposition::Claim { sentence, .. } if sentence.text == "Trade is a kind of Transaction"
        ));
        let Proposition::Cert {
            targets: canceled_targets,
            requires: canceled_reqs,
            ..
        } = &impl_compiled.claims[1]
        else {
            panic!("expected Cert for canceled");
        };
        assert_eq!(
            canceled_targets[0].text,
            "For all t, t is canceled if and only if there exists n such that (t has n cancels stacked on it and n is odd)."
        );
        assert!(canceled_reqs.is_empty());

        let Proposition::Cert {
            targets: erased_targets,
            requires: erased_reqs,
            ..
        } = &impl_compiled.claims[2]
        else {
            panic!("expected Cert for erased");
        };
        assert_eq!(
            erased_targets[0].text,
            "For all t and c, t is erased by c if and only if (t is canceled and t is canceled by c)."
        );
        assert_eq!(
            erased_reqs,
            &vec!["certify:acme.ibor.impl:acme.ibor/canceled".to_string()]
        );

        // Layer 3: ground against certified sense asks provenance and imports layer 2 certs.
        let run_compiled = link(&files[2].program, Some(files[2].path.as_str()), &files).unwrap();
        let Proposition::Cert {
            targets: ground_targets,
            requires: ground_reqs,
            imported,
            ..
        } = &run_compiled.claims[1]
        else {
            panic!("expected Cert for ground");
        };
        assert_eq!(
            ground_targets[0].text,
            "For all t and c, if erased.csv supports that t is erased by c, then t is erased by c."
        );
        assert_eq!(
            ground_reqs,
            &vec!["certify:acme.ibor.impl:acme.ibor/erased".to_string()]
        );
        assert_eq!(imported.len(), 2);

        // Layer 4: downstream brief rebases all paths and inherits assumptions.
        let decision = parse(
            r#"
(ns acme.decision
  (:require
    [acme.ibor :refer :all]
    [acme.ibor.run]))
(name TX1)
(name C1)
(and (erased TX1 C1))
"#,
        )
        .unwrap();
        let dec = link(&decision, Some("analysis/decision.brief"), &files).unwrap();
        assert_eq!(dec.assumptions, vec!["erased.csv came from ibor.py"]);
        let Proposition::And(xs) = &dec.claims[0] else {
            panic!("and");
        };
        let Proposition::Grounded {
            witness,
            certificate,
            ..
        } = &xs[0]
        else {
            panic!("expected Grounded");
        };
        assert_eq!(witness, &vec!["../run/erased.csv".to_string()]);
        assert_eq!(certificate.len(), 2);
        let Proposition::Cert { imported, .. } = &certificate[1] else {
            panic!("expected ground Cert");
        };
        assert_eq!(imported.len(), 2);
        let Proposition::Cert {
            support: Support::Witness { paths },
            ..
        } = &imported[0]
        else {
            panic!("expected imported canceled Cert");
        };
        assert_eq!(paths, &vec!["../impl/cancel_chains.py".to_string()]);
    }
}
