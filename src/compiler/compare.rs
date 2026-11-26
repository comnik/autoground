use std::collections::{HashMap, HashSet};

use crate::syntax::{Program, Proposition};

use super::link::{build_index, dependency_order, environment, link, link_module};
use super::scan::Loaded;
use super::{compile_module, CompileError, Compiled, CompiledModule, SenseKey};

#[derive(Debug, Clone)]
pub struct AgainstReport {
    pub compiled: Compiled,
    pub extra_claims: Vec<Proposition>,
    pub assumptions: Vec<String>,
    pub missing_certifies: Vec<String>,
    pub unaccepted_assumptions: Vec<String>,
}

impl AgainstReport {
    pub fn has_failures(&self) -> bool {
        !self.missing_certifies.is_empty() || !self.unaccepted_assumptions.is_empty()
    }

    pub fn format_failures(&self) -> String {
        let mut out = String::new();
        if !self.missing_certifies.is_empty() {
            out.push_str("missing certify:\n");
            for item in &self.missing_certifies {
                out.push_str("  ");
                out.push_str(item);
                out.push('\n');
            }
        }
        if !self.unaccepted_assumptions.is_empty() {
            out.push_str("unaccepted assumptions:\n");
            for item in &self.unaccepted_assumptions {
                out.push_str("  ");
                out.push_str(item);
                out.push('\n');
            }
        }
        out
    }
}

/// Compile `cert` against a trusted `ontology` and verify full `define` coverage
/// plus assumption allowlisting (before any oracle calls).
pub fn compare_against(
    cert: &Program,
    cert_path: Option<&str>,
    ontology: &Program,
    ontology_path: &str,
    accepted_assumptions: Option<(&Program, Option<&str>)>,
    scanned_files: &[Loaded],
) -> Result<AgainstReport, CompileError> {
    let Some(onto_ns) = &ontology.namespace else {
        return Err(CompileError {
            message: "`--against` requires a namespaced ontology brief".into(),
        });
    };

    // Pin the trusted ontology namespace ahead of workspace-scanned files so a
    // local copy in the certificate's workspace cannot override it.
    let mut trusted_files: Vec<Loaded> = scanned_files
        .iter()
        .filter(|file| {
            file.program
                .namespace
                .as_ref()
                .is_none_or(|ns| ns.name != onto_ns.name)
        })
        .cloned()
        .collect();
    trusted_files.push(Loaded {
        path: ontology_path.to_string(),
        program: ontology.clone(),
    });

    let (rule_obligations, fact_obligations) =
        collect_ontology_obligations(ontology, ontology_path, &trusted_files)?;
    if rule_obligations.is_empty() && fact_obligations.is_empty() {
        return Err(CompileError {
            message: format!("ontology `{}` declares no definitions to certify", onto_ns.name),
        });
    }

    let (cert_module, cert_imports) = link_module(cert, cert_path, &trusted_files)?;

    let mut extra_claims = Vec::new();
    let mut seen_extra_certs = HashSet::new();
    let mut assumptions = cert_module.compiled.assumptions.clone();
    let mut missing_certifies = Vec::new();

    for key in &rule_obligations {
        if cert_module.certifies.contains_key(key) {
            continue;
        }
        if let Some(imported) = cert_imports.certifies.get(key) {
            if seen_extra_certs.insert(imported.cert_id.clone()) {
                extra_claims.push(Proposition::Scope {
                    body: imported.certificate.clone(),
                });
                for assumption in &imported.assumptions {
                    if !assumptions.contains(assumption) {
                        assumptions.push(assumption.clone());
                    }
                }
            }
            continue;
        }
        let label = if key.owner == onto_ns.name {
            key.name.clone()
        } else {
            key.display()
        };
        missing_certifies.push(label);
    }

    for fact in &fact_obligations {
        if cert_module.certified_facts.contains_key(fact) {
            continue;
        }
        if let Some(imported) = cert_imports.certified_facts.get(fact) {
            if seen_extra_certs.insert(imported.cert_id.clone()) {
                extra_claims.push(Proposition::Scope {
                    body: imported.certificate.clone(),
                });
                for assumption in &imported.assumptions {
                    if !assumptions.contains(assumption) {
                        assumptions.push(assumption.clone());
                    }
                }
            }
            continue;
        }
        missing_certifies.push(fact.clone());
    }

    let accepted: HashSet<String> = match accepted_assumptions {
        Some((prog, path)) => link(prog, path, &trusted_files)?
            .assumptions
            .into_iter()
            .collect(),
        None => HashSet::new(),
    };

    let unaccepted_assumptions: Vec<String> = assumptions
        .iter()
        .filter(|a| !accepted.contains(*a))
        .cloned()
        .collect();

    Ok(AgainstReport {
        compiled: cert_module.compiled,
        extra_claims,
        assumptions,
        missing_certifies,
        unaccepted_assumptions,
    })
}

fn collect_ontology_obligations(
    ontology: &Program,
    ontology_path: &str,
    files: &[Loaded],
) -> Result<(Vec<SenseKey>, Vec<String>), CompileError> {
    let index = build_index(ontology, Some(ontology_path), files)?;
    let order = dependency_order(ontology, &index)?;
    let mut modules: HashMap<String, CompiledModule> = HashMap::new();
    let mut rules = Vec::new();
    let mut facts = Vec::new();
    let mut seen_rules = HashSet::new();
    let mut seen_facts = HashSet::new();

    for ns in &order {
        let item = &index[ns];
        let imports = environment(item.program, item.path, &index, &modules)?;
        let module = compile_module(item.program, &imports).map_err(|err| CompileError {
            message: format!("in `{ns}`: {}", err.message),
        })?;
        for key in &module.define_order {
            if seen_rules.insert(key.clone()) {
                rules.push(key.clone());
            }
        }
        for fact in &module.fact_order {
            if seen_facts.insert(fact.clone()) {
                facts.push(fact.clone());
            }
        }
        modules.insert(ns.clone(), module);
    }

    let imports = environment(ontology, ontology_path, &index, &modules)?;
    let root = compile_module(ontology, &imports)?;
    for key in &root.define_order {
        if seen_rules.insert(key.clone()) {
            rules.push(key.clone());
        }
    }
    for fact in &root.fact_order {
        if seen_facts.insert(fact.clone()) {
            facts.push(fact.clone());
        }
    }
    Ok((rules, facts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse;

    fn loaded(path: &str, src: &str) -> Loaded {
        Loaded {
            path: path.into(),
            program: parse(src).unwrap(),
        }
    }

    #[test]
    fn compare_checks_full_coverage_assumption_allowlist_and_trusted_pinning() {
        let trusted_ontology = parse(
            r#"
(ns acme.ontology)
(name Customer)
(name EnterpriseCustomer)
(sense is_a "{Sub} is a sub-type of {Super}")
(sense active "{c} is active")
(sense hv "{c} is high value")
(define (is_a EnterpriseCustomer Customer))
(define (active c) ("{c} has paid order" c))
(define (hv c) (and (active c) ("{c} has sales > $25M" c)))
"#,
        )
        .unwrap();

        // A tampered local ontology in the workspace drops `hv` and `is_a`,
        // but `--against` pins `trusted_ontology` instead.
        let tampered_workspace = vec![loaded(
            "ontology.brief",
            r#"
(ns acme.ontology)
(sense active "{c} is active")
(define (active c) ("{c} has paid order" c))
"#,
        )];

        // Incomplete certificate + smuggled assumption: fails structurally.
        let partial_cert = parse(
            r#"
(ns impl.cert (:require [acme.ontology :refer :all]))
(assume "views.sql is correct")
(certify (active c) (witness "views.sql"))
"#,
        )
        .unwrap();
        let report = compare_against(
            &partial_cert,
            Some("impl/cert.brief"),
            &trusted_ontology,
            "trusted/ontology.brief",
            None,
            &tampered_workspace,
        )
        .unwrap();
        assert!(report.has_failures());
        assert_eq!(
            report.missing_certifies,
            vec!["hv", "EnterpriseCustomer is a sub-type of Customer"]
        );
        assert_eq!(report.unaccepted_assumptions, vec!["views.sql is correct"]);

        // Complete certificate with an accepted assumption log passes structural audit.
        let full_cert = parse(
            r#"
(ns impl.cert (:require [acme.ontology :refer :all]))
(assume "Postgres EXISTS checks non-emptiness")
(certify (is_a EnterpriseCustomer Customer) (witness "types.py"))
(certify (active c)
  (and
    (claim "active view uses EXISTS" (witness "views.sql"))
    "Postgres EXISTS checks non-emptiness"))
(certify (hv c) (witness "views.sql"))
"#,
        )
        .unwrap();
        let accepted = parse(
            r#"
(assume "Postgres EXISTS checks non-emptiness")
(assume "Extra unused accepted assumption is fine")
"#,
        )
        .unwrap();
        let ok = compare_against(
            &full_cert,
            Some("impl/cert.brief"),
            &trusted_ontology,
            "trusted/ontology.brief",
            Some((&accepted, Some("impl/assumptions.brief"))),
            &tampered_workspace,
        )
        .unwrap();
        assert!(!ok.has_failures(), "{}", ok.format_failures());

        // Shadowing an ontology sense locally does NOT satisfy coverage.
        let shadowed = parse(
            r#"
(ns impl.cheat (:require [acme.ontology :refer [Customer EnterpriseCustomer is_a hv]]))
(sense active "{c} is active")
(define (active c) ("{c} is anything" c))
(certify (is_a EnterpriseCustomer Customer) (witness "types.py"))
(certify (active c) (witness "views.sql"))
(certify (hv c) (witness "views.sql"))
"#,
        )
        .unwrap();
        let cheat_report = compare_against(
            &shadowed,
            Some("impl/cheat.brief"),
            &trusted_ontology,
            "trusted/ontology.brief",
            None,
            &tampered_workspace,
        )
        .unwrap();
        assert_eq!(cheat_report.missing_certifies, vec!["active"]);
    }

    #[test]
    fn compare_collects_certifies_from_required_sub_certificates() {
        let ontology = parse(
            r#"
(ns acme.ontology)
(name A)
(name B)
(sense is_a "{Sub} is a kind of {Super}")
(sense p "{x} holds")
(define (is_a A B))
(define (p x) ("{x} is positive" x))
"#,
        )
        .unwrap();
        let files = vec![
            loaded(
                "impl/facts.brief",
                r#"
(ns impl.facts (:require [acme.ontology :refer :all]))
(certify (is_a A B) (witness "types.py"))
"#,
            ),
            loaded(
                "impl/rules.brief",
                r#"
(ns impl.rules (:require [acme.ontology :refer :all]))
(certify (p x) (witness "rules.py"))
"#,
            ),
        ];
        let umbrella = parse(
            r#"
(ns impl.all
  (:require
    [impl.facts]
    [impl.rules]))
"#,
        )
        .unwrap();
        let report = compare_against(
            &umbrella,
            Some("impl/all.brief"),
            &ontology,
            "ontology.brief",
            None,
            &files,
        )
        .unwrap();
        assert!(!report.has_failures(), "{}", report.format_failures());
        assert_eq!(report.extra_claims.len(), 2);
    }
}
