use autoground::edb::Store;
use autoground::eval::{run, Outcome};
use autoground::navigator::{
    Arg, Demand, FixtureJudge, GroundStep, Navigator, Need, TreeNavigator,
};
use autoground::report::step::format_trace;
use autoground::{compile, parse, parse_atom};

fn compiled_layered() -> autoground::Compiled {
    let src = std::fs::read_to_string("examples/layered.ag").unwrap();
    compile(&parse(&src).unwrap()).unwrap()
}

fn ground(repo: &str) -> (Outcome, Store, Vec<String>) {
    let compiled = compiled_layered();
    let mut store = Store::new();
    let mut nav = TreeNavigator::heuristic(repo);
    let result = run(&compiled, &mut store, Some(&mut nav));
    (result.outcome, store, nav.documents)
}

fn assert_no_decoy(docs: &[String]) {
    assert!(
        docs.iter().all(|p| !p.contains("decoy")),
        "decoy document was opened: {docs:?}"
    );
}

#[test]
fn ground_frontend_on_split_has_readable_steps() {
    let mut store = Store::new();
    let mut nav = TreeNavigator::heuristic("examples/toy-split");
    let demand = Demand {
        pred: "component".into(),
        args: vec![Arg::Bound("Frontend".into())],
        need: Need::Truth,
    };
    let answer = nav.answer(&demand, &store);
    for (c, e) in &answer.bindings {
        store.bind(c, e);
    }
    assert_eq!(
        store.bindings_for("Frontend"),
        vec!["ui/app.py".to_string()]
    );
    assert!(nav
        .trace
        .iter()
        .any(|s| matches!(s, GroundStep::Listing { .. })));
    assert!(nav
        .trace
        .iter()
        .any(|s| matches!(s, GroundStep::Result { .. })));
    let text = format_trace(&nav.trace, "toy-split");
    assert!(text.contains("toy-split/"));
    assert!(text.contains("ui"));
    for (i, step) in nav.trace.iter().enumerate() {
        let GroundStep::Document { path, .. } = step else {
            continue;
        };
        let listed = nav.trace[..i].iter().any(|s| {
            matches!(s, GroundStep::Listing { children, .. } if children.iter().any(|c| c.rel_path == *path))
        });
        assert!(
            listed,
            "document {path} appeared before the listing that flagged it"
        );
    }
}

#[test]
fn locate_is_generic_over_predicate_name() {
    let mut nav = TreeNavigator::heuristic("examples/toy-layered");
    let demand = Demand {
        pred: "Service".into(),
        args: vec![Arg::Bound("Frontend".into())],
        need: Need::Truth,
    };
    let answer = nav.answer(&demand, &Store::new());
    assert!(
        answer
            .bindings
            .iter()
            .any(|(c, e)| c == "Frontend" && e.starts_with("frontend/")),
        "expected a file under frontend/, got {:?}",
        answer.bindings
    );
}

#[test]
fn parse_ground_atom_question_mark() {
    let (pred, args) = parse_atom("(component ?x)").unwrap();
    assert_eq!(pred, "component");
    assert!(matches!(&args[0], autoground::syntax::Term::Var(v) if v == "x"));
}

#[test]
fn toy_layered_grounds() {
    let (outcome, store, docs) = ground("examples/toy-layered");
    assert_no_decoy(&docs);
    assert_eq!(outcome, Outcome::Grounded);
    assert!(store
        .bindings_for("Frontend")
        .iter()
        .all(|p| p.starts_with("frontend/")));
    assert!(!store.bindings_for("Frontend").is_empty());
    assert!(store
        .bindings_for("Backend")
        .iter()
        .all(|p| p.starts_with("backend/")));
    assert!(!store.bindings_for("Backend").is_empty());
}

#[test]
fn toy_cycle_violates() {
    let (outcome, _store, docs) = ground("examples/toy-cycle");
    assert_no_decoy(&docs);
    assert_eq!(outcome, Outcome::Violated);
}

#[test]
fn toy_unknown_is_ungrounded() {
    let (outcome, store, _docs) = ground("examples/toy-unknown");
    assert_eq!(outcome, Outcome::Ungrounded);
    assert!(store.bindings_for("Frontend").is_empty());
}

#[test]
fn jev_collapsed_trace_does_not_bind_both_to_one_file() {
    let compiled = compiled_layered();
    let mut store = Store::new();
    let judge = FixtureJudge::load("tests/fixtures/jev-split-collapsed.json").unwrap();
    let mut nav = TreeNavigator::new("examples/toy-split", Box::new(judge));
    let _ = run(&compiled, &mut store, Some(&mut nav));
    let front = store.bindings_for("Frontend");
    let back = store.bindings_for("Backend");
    assert_ne!(
        front, back,
        "Frontend and Backend must not collapse to the same path: {front:?}"
    );
    assert!(
        !front.iter().any(|p| p.ends_with("client.py")),
        "Frontend should not bind a Backend source file: {front:?}"
    );
    assert!(
        front.iter().any(|p| p.ends_with("app.py")),
        "Frontend should bind ui/app.py: {front:?}"
    );
}

#[test]
fn toy_split_binds_two_backend_roots() {
    let (outcome, store, docs) = ground("examples/toy-split");
    assert_no_decoy(&docs);
    assert_eq!(outcome, Outcome::Grounded);
    assert_eq!(
        store.bindings_for("Frontend"),
        vec!["ui/app.py".to_string()]
    );
    let back = store.bindings_for("Backend");
    assert!(
        back.iter().any(|p| p.starts_with("api/")),
        "Backend should include api/ files: {back:?}"
    );
    assert!(
        back.iter().any(|p| p.starts_with("server/")),
        "Backend should include server/ files: {back:?}"
    );
    assert!(
        back.iter()
            .all(|p| p.starts_with("api/") || p.starts_with("server/")),
        "Backend should only bind api/ and server/ files: {back:?}"
    );
}

#[test]
fn toy_layered_from_fixture() {
    let compiled = compiled_layered();
    let mut store = Store::new();
    let judge = FixtureJudge::load("tests/fixtures/toy-layered.json").unwrap();
    let mut nav = TreeNavigator::new("examples/toy-layered", Box::new(judge));
    let result = run(&compiled, &mut store, Some(&mut nav));
    assert_eq!(result.outcome, Outcome::Grounded);
    assert!(nav.documents.iter().all(|p| !p.contains("decoy")));
}

#[test]
fn toy_file_binds_app_js() {
    let (outcome, store, docs) = ground("examples/toy-file");
    assert_no_decoy(&docs);
    assert_eq!(outcome, Outcome::Grounded);
    assert_eq!(
        store.bindings_for("Frontend"),
        vec!["src/app.js".to_string()]
    );
    assert_eq!(
        store.bindings_for("Backend"),
        vec!["src/api.js".to_string()]
    );
}

#[test]
fn forall_components_have_a_file_on_toy_split() {
    let src = r#"
(declare component "{String} is a component in the codebase")
(declare file "{String} is a file in the codebase")
(declare belongs_to "{file} belongs to {component}")
(require
  (forall [c]
    (implies
      (component c)
      (exists [f]
        (and
          (file f)
          (belongs_to f c))))))
"#;
    let compiled = compile(&parse(src).unwrap()).unwrap();
    let mut store = Store::new();
    let mut nav = TreeNavigator::heuristic("examples/toy-split");
    let result = run(&compiled, &mut store, Some(&mut nav));
    assert_eq!(result.outcome, Outcome::Grounded);
    assert_eq!(result.requires[0].value, autoground::eval::Value::True);
    assert!(
        !store.facts_for("component").is_empty(),
        "forall should generate components, got {:?}",
        store.facts_for("component")
    );
}
