//! Mermaid diagram of a brief.
//!
//! Names are nodes, binary sense applications are edges, and details are
//! subgraphs. A claim is not a box. A closing `senses` subgraph verbalizes
//! each reading. One oracle request classifies each name as a component or as
//! data, and chooses a priority. The state of that request is the verbalized
//! context, plus the intent of the scope that declared the name or the sense.
//! Shape and paint are fixed from those answers. A contradicted claim draws
//! its edge with a red dashed stroke.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{json, Value as Json};

use crate::compiler::{extend_scope, NameOrigin, Vocabulary};
use crate::oracle::{Answer, Classification, Oracle, OracleError, QuestionKind, State};
use crate::syntax::template::apply;
use crate::syntax::{Arg, Form, Formula, Program, Reading, Sense};

/// `…/trace.jsonl` → `…/diagram.mmd`.
pub fn diagram_path(trace: &Path) -> PathBuf {
    match trace.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join("diagram.mmd"),
        _ => PathBuf::from("diagram.mmd"),
    }
}

/// Write [`render`] to [`diagram_path`].
pub fn write_diagram(trace: &Path, diagram: &Diagram, styles: &Styles) -> io::Result<PathBuf> {
    let path = diagram_path(trace);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(&path, render(diagram, styles))?;
    Ok(path)
}

/// The named picture of one brief.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagram {
    root: Scope,
    legend: Vec<LegendRow>,
}

impl Diagram {
    /// A Jev run asks for style when the picture has a name or a declared sense.
    pub fn has_style_questions(&self) -> bool {
        style_questions(self, "").is_some()
    }

    /// Positive edges whose sentence is in `sentences` take the contradicted stroke.
    pub fn mark_contradicted(&mut self, sentences: &HashSet<String>) {
        mark_contradicted(&mut self.root, sentences);
    }
}

fn mark_contradicted(scope: &mut Scope, sentences: &HashSet<String>) {
    for edge in &mut scope.edges {
        if !edge.negated && sentences.contains(&edge.sentence) {
            edge.contradicted = true;
        }
    }
    for child in &mut scope.children {
        mark_contradicted(child, sentences);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Scope {
    /// A detail. The root brief is not boxed.
    boxed: bool,
    /// The detail's own intent. Absent when the detail states none.
    title: Option<String>,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    children: Vec<Scope>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Node {
    id: String,
    name: String,
    /// Intent of the scope that declared this name, enclosing intents first.
    intent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Edge {
    from: String,
    to: String,
    label: String,
    /// Filled reading this edge states. A `not` keeps the positive sentence.
    sentence: String,
    negated: bool,
    contradicted: bool,
    /// Declared sense this edge takes its priority from. Inline readings have none.
    sense: Option<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LegendRow {
    id: String,
    name: Option<String>,
    reading: String,
    /// Intent of the scope that declared this sense.
    intent: Option<String>,
}

/// Kind and priority for the nodes and senses in one diagram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Styles {
    kinds: HashMap<String, Kind>,
    name_priority: HashMap<String, Priority>,
    sense_priority: HashMap<(String, String), Priority>,
}

impl Styles {
    /// Rectangle nodes, normal priority. No oracle.
    pub fn plain() -> Self {
        Self {
            kinds: HashMap::new(),
            name_priority: HashMap::new(),
            sense_priority: HashMap::new(),
        }
    }

    /// One same-state batch. An unknown choice leaves that item on the plain default.
    ///
    /// `context` is the green proof. A scope's intent joins it when one is declared.
    pub fn ask(oracle: &mut impl Oracle, diagram: &Diagram, context: &str) -> Self {
        let Some(batch) = style_questions(diagram, context) else {
            return Self::plain();
        };
        let tickets: Vec<_> = batch
            .questions
            .iter()
            .cloned()
            .map(|question| oracle.submit(question))
            .collect();
        let answers = oracle.wait_many(&tickets);
        from_answers(&batch.names, &batch.senses, &answers)
    }

    fn shape_of(&self, name: &str) -> Shape {
        self.kinds
            .get(name)
            .copied()
            .map(Kind::shape)
            .unwrap_or(Shape::Rectangle)
    }

    fn priority_of_name(&self, name: &str) -> Priority {
        self.name_priority
            .get(name)
            .copied()
            .unwrap_or(Priority::Normal)
    }

    fn priority_of_sense(&self, sense: Option<&(String, String)>) -> Priority {
        sense
            .and_then(|key| self.sense_priority.get(key).copied())
            .unwrap_or(Priority::Normal)
    }
}

/// What a name is, as far as the picture is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Component,
    Data,
}

impl Kind {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "component" => Self::Component,
            "data" => Self::Data,
            _ => return None,
        })
    }

    fn shape(self) -> Shape {
        match self {
            Self::Component => Shape::Rounded,
            Self::Data => Shape::Cylinder,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Rectangle,
    Rounded,
    Cylinder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Priority {
    Deemphasized,
    Normal,
    Highlighted,
}

impl Priority {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "deemphasized" => Self::Deemphasized,
            "normal" => Self::Normal,
            "highlighted" => Self::Highlighted,
            _ => return None,
        })
    }

    fn class_name(self) -> &'static str {
        match self {
            Self::Deemphasized => "deemphasized",
            Self::Normal => "normal",
            Self::Highlighted => "highlighted",
        }
    }

    fn arrow(self) -> &'static str {
        match self {
            Self::Deemphasized => "-.->",
            Self::Normal => "-->",
            Self::Highlighted => "==>",
        }
    }

    fn stroke(self) -> &'static str {
        match self {
            Self::Deemphasized => "stroke:#a1a1aa,stroke-width:1px",
            Self::Normal => "stroke:#334155,stroke-width:1.5px",
            Self::Highlighted => "stroke:#b45309,stroke-width:2.5px",
        }
    }
}

const CLASS_DEEMPHASIZED: &str =
    "classDef deemphasized fill:#f4f4f5,stroke:#a1a1aa,stroke-width:1px,stroke-dasharray: 4 3";
const CLASS_NORMAL: &str = "classDef normal fill:#f8fafc,stroke:#334155,stroke-width:1.5px";
const CLASS_HIGHLIGHTED: &str =
    "classDef highlighted fill:#fef3c7,stroke:#b45309,stroke-width:2.5px";
const CONTRADICTED_STROKE: &str = "stroke:#dc2626,stroke-width:1.5px,stroke-dasharray: 4 3";

/// Flowchart text. Priority picks the class, the arrow, and `linkStyle`.
pub fn render(diagram: &Diagram, styles: &Styles) -> String {
    let mut out = String::from("flowchart TD\n");
    out.push_str(&format!("  {CLASS_DEEMPHASIZED}\n"));
    out.push_str(&format!("  {CLASS_NORMAL}\n"));
    out.push_str(&format!("  {CLASS_HIGHLIGHTED}\n"));
    let mut strokes = Vec::new();
    let mut details = 0usize;
    render_scope(
        &diagram.root,
        1,
        styles,
        &mut details,
        &mut strokes,
        &mut out,
    );
    for (index, stroke) in strokes.iter().enumerate() {
        out.push_str(&format!("  linkStyle {index} {stroke}\n"));
    }
    out.push_str("  subgraph senses [\"senses\"]\n");
    for row in &diagram.legend {
        let text = match &row.name {
            Some(name) => format!("{name} — {}", row.reading),
            None => row.reading.clone(),
        };
        out.push_str(&format!("    {}[{}]\n", row.id, quote(&text)));
    }
    out.push_str("  end\n");
    out
}

fn render_scope(
    scope: &Scope,
    depth: usize,
    styles: &Styles,
    details: &mut usize,
    strokes: &mut Vec<&'static str>,
    out: &mut String,
) {
    let pad = "  ".repeat(depth);
    let inner = if scope.boxed { depth + 1 } else { depth };
    if scope.boxed {
        *details += 1;
        match &scope.title {
            Some(title) => {
                out.push_str(&format!("{pad}subgraph d{details} [{}]\n", quote(title)));
            }
            None => out.push_str(&format!("{pad}subgraph d{details}\n")),
        }
    }
    let pad_in = "  ".repeat(inner);
    for node in &scope.nodes {
        let body = shaped(&node.id, &node.name, styles.shape_of(&node.name));
        let class = styles.priority_of_name(&node.name).class_name();
        out.push_str(&format!("{pad_in}{body}:::{class}\n"));
    }
    for edge in &scope.edges {
        let (arrow, stroke) = if edge.contradicted {
            ("-.->", CONTRADICTED_STROKE)
        } else {
            let priority = styles.priority_of_sense(edge.sense.as_ref());
            (priority.arrow(), priority.stroke())
        };
        strokes.push(stroke);
        out.push_str(&format!(
            "{pad_in}{} {arrow}|{}| {}\n",
            edge.from,
            quote(&edge.label),
            edge.to
        ));
    }
    for child in &scope.children {
        render_scope(child, inner, styles, details, strokes, out);
    }
    if scope.boxed {
        out.push_str(&format!("{pad}end\n"));
    }
}

fn shaped(id: &str, label: &str, shape: Shape) -> String {
    let label = quote(label);
    match shape {
        Shape::Rectangle => format!("{id}[{label}]"),
        Shape::Rounded => format!("{id}({label})"),
        Shape::Cylinder => format!("{id}[({label})]"),
    }
}

fn quote(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("#quot;"),
            '\n' => out.push(' '),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

struct Batch {
    names: Vec<String>,
    senses: Vec<(String, String)>,
    questions: Vec<Classification>,
}

/// Names, then senses. Two choices per name (kind, priority) and one per sense.
/// A name or sense takes the intent of the scope that declared it. With no
/// intent anywhere, the state is the context text alone.
fn style_questions(diagram: &Diagram, context: &str) -> Option<Batch> {
    let named = inventory_names(&diagram.root);
    let senses: Vec<(String, String, Option<String>)> = diagram
        .legend
        .iter()
        .filter_map(|row| {
            row.name
                .as_ref()
                .map(|name| (name.clone(), row.reading.clone(), row.intent.clone()))
        })
        .collect();
    if named.is_empty() && senses.is_empty() {
        return None;
    }
    let mut questions = Vec::new();
    for (name, intent) in &named {
        let state = style_state(context, intent);
        questions.push(choice(
            &state,
            json!({
                "name": name,
                "question": "Is this name a component or data?",
            }),
            kind_criteria(),
        ));
        let prominence = if intent.is_some() {
            "How prominent should this name be given the `intent` behind the diagram?"
        } else {
            "How prominent should this name be?"
        };
        questions.push(choice(
            &state,
            json!({
                "name": name,
                "question": prominence,
            }),
            name_priority_criteria(),
        ));
    }
    for (name, reading, intent) in &senses {
        let state = style_state(context, intent);
        let strength = if intent.is_some() {
            "How strongly should this relation read given the `intent` behind the diagram?"
        } else {
            "How strongly should this relation read?"
        };
        questions.push(choice(
            &state,
            json!({
                "name": name,
                "reading": reading,
                "question": strength,
            }),
            sense_priority_criteria(),
        ));
    }
    Some(Batch {
        names: named.into_iter().map(|(name, _)| name).collect(),
        senses: senses
            .into_iter()
            .map(|(name, reading, _)| (name, reading))
            .collect(),
        questions,
    })
}

/// Context alone, or context plus the declaring scope's intent chain.
fn style_state(context: &str, intent: &Option<String>) -> State {
    match intent {
        Some(intent) => State::new(json!({ "context": context, "intent": intent })),
        None => State::new(json!(context)),
    }
}

fn choice(state: &State, instructions: Json, criteria: Json) -> Classification {
    Classification {
        state: state.clone(),
        kind: QuestionKind::Choice,
        instructions,
        criteria,
    }
}

fn kind_criteria() -> Json {
    json!({
        "component": "something that does work",
        "data": "a store, record, or value that is kept",
    })
}

fn name_priority_criteria() -> Json {
    json!({
        "deemphasized": "background, easy to look past",
        "normal": "an ordinary part of the picture",
        "highlighted": "a reader should see this first",
    })
}

fn sense_priority_criteria() -> Json {
    json!({
        "deemphasized": "a relation that should sit quietly",
        "normal": "an ordinary relation",
        "highlighted": "a relation a reader should see first",
    })
}

fn inventory_names(scope: &Scope) -> Vec<(String, Option<String>)> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    collect_names(scope, &mut names, &mut seen);
    names
}

/// Root first, and a scope's declared nodes before the copies an edge adds.
/// The first sight of a spelling is the scope that declared it.
fn collect_names(
    scope: &Scope,
    names: &mut Vec<(String, Option<String>)>,
    seen: &mut HashSet<String>,
) {
    for node in &scope.nodes {
        if seen.insert(node.name.clone()) {
            names.push((node.name.clone(), node.intent.clone()));
        }
    }
    for child in &scope.children {
        collect_names(child, names, seen);
    }
}

fn from_answers(
    names: &[String],
    senses: &[(String, String)],
    answers: &[Result<Answer, OracleError>],
) -> Styles {
    let mut styles = Styles::plain();
    let mut index = 0;
    for name in names {
        if let Some(kind) = choice_at(answers, index).and_then(Kind::parse) {
            styles.kinds.insert(name.clone(), kind);
        }
        index += 1;
        if let Some(priority) = choice_at(answers, index).and_then(Priority::parse) {
            styles.name_priority.insert(name.clone(), priority);
        }
        index += 1;
    }
    for (name, reading) in senses {
        if let Some(priority) = choice_at(answers, index).and_then(Priority::parse) {
            styles
                .sense_priority
                .insert((name.clone(), reading.clone()), priority);
        }
        index += 1;
    }
    styles
}

fn choice_at<'a>(answers: &'a [Result<Answer, OracleError>], index: usize) -> Option<&'a str> {
    match answers.get(index) {
        Some(Ok(answer)) => Some(answer.choice.as_str()),
        _ => None,
    }
}

/// Names, binary applications, and details, read from the program before
/// claims flatten into sentences.
pub fn diagram_of(program: &Program, vocab: &Vocabulary) -> Diagram {
    let mut legend = LegendAcc::default();
    let built = bindings(&program.forms);
    let root_intent = intent_chain(None, own_intent(&program.forms).as_deref());
    let mut root = RawScope::new(Vec::new());
    root.declared = built.order;
    root.intent = root_intent.clone();
    let mut stack = vec![built.layer];
    walk_forms(
        &program.forms,
        false,
        &root_intent,
        &mut stack,
        vocab,
        &mut root,
        &mut legend,
    );
    rehome(&mut root);
    assemble(root, legend.finish())
}

fn own_intent(forms: &[Form]) -> Option<String> {
    forms.iter().find_map(|form| match form {
        Form::Intent(text) => Some(text.clone()),
        _ => None,
    })
}

/// Enclosing intents first. A scope with no intent of its own keeps the parent's.
fn intent_chain(parent: Option<&str>, own: Option<&str>) -> Option<String> {
    match (parent, own) {
        (Some(parent), Some(own)) => Some(format!("{parent}\n{own}")),
        (Some(parent), None) => Some(parent.to_string()),
        (None, Some(own)) => Some(own.to_string()),
        (None, None) => None,
    }
}

#[derive(Default)]
struct LegendAcc {
    seen: HashSet<(Option<String>, String)>,
    rows: Vec<LegendDraft>,
}

struct LegendDraft {
    name: Option<String>,
    reading: String,
    intent: Option<String>,
}

impl LegendAcc {
    fn push_sense(&mut self, name: &str, reading: &str, intent: Option<String>) {
        let key = (Some(name.to_string()), reading.to_string());
        if self.seen.insert(key) {
            self.rows.push(LegendDraft {
                name: Some(name.to_string()),
                reading: reading.to_string(),
                intent,
            });
        }
    }

    fn push_reading(&mut self, reading: &str) {
        let key = (None, reading.to_string());
        if self.seen.insert(key) {
            self.rows.push(LegendDraft {
                name: None,
                reading: reading.to_string(),
                intent: None,
            });
        }
    }

    fn finish(self) -> Vec<LegendDraft> {
        self.rows
    }
}

struct Layer {
    names: HashSet<String>,
    senses: HashMap<String, Sense>,
}

struct Built {
    order: Vec<String>,
    layer: Layer,
}

fn bindings(forms: &[Form]) -> Built {
    let mut order = Vec::new();
    let mut names = HashSet::new();
    let mut senses = HashMap::new();
    for form in forms {
        match form {
            Form::Name(name) => {
                order.push(name.clone());
                names.insert(name.clone());
            }
            Form::Sense(sense) => {
                senses.insert(sense.name.clone(), sense.clone());
            }
            Form::Intent(_) | Form::Assume(_) | Form::Detail(_) | Form::Formula(_) => {}
        }
    }
    Built {
        order,
        layer: Layer { names, senses },
    }
}

struct RawScope {
    path: Vec<usize>,
    boxed: bool,
    title: Option<String>,
    /// Intent chain used for style questions in this scope.
    intent: Option<String>,
    declared: Vec<String>,
    edges: Vec<RawEdge>,
    children: Vec<RawScope>,
}

impl RawScope {
    fn new(path: Vec<usize>) -> Self {
        Self {
            path,
            boxed: false,
            title: None,
            intent: None,
            declared: Vec::new(),
            edges: Vec::new(),
            children: Vec::new(),
        }
    }
}

struct RawEdge {
    from: End,
    to: End,
    label: String,
    sentence: String,
    negated: bool,
    sense: Option<(String, String)>,
    /// Scope the edge is drawn in. A prefix of the scope that wrote it.
    home: Vec<usize>,
}

#[derive(Clone)]
enum End {
    Declared(String),
    Imported(String),
    Literal(String),
}

fn walk_forms(
    forms: &[Form],
    negated: bool,
    root_intent: &Option<String>,
    stack: &mut Vec<Layer>,
    vocab: &Vocabulary,
    scope: &mut RawScope,
    legend: &mut LegendAcc,
) {
    for form in forms {
        match form {
            Form::Name(_) | Form::Intent(_) => {}
            Form::Sense(sense) => {
                legend.push_sense(&sense.name, &sense.template, scope.intent.clone());
            }
            Form::Assume(sentence) => {
                walk_formula(sentence, negated, root_intent, stack, vocab, scope, legend)
            }
            Form::Formula(formula) => {
                walk_formula(formula, negated, root_intent, stack, vocab, scope, legend)
            }
            Form::Detail(body) => {
                if extend_scope(
                    body,
                    &visible_names(stack, vocab),
                    &visible_senses(stack, vocab),
                )
                .is_err()
                {
                    return;
                }
                let built = bindings(body);
                let mut path = scope.path.clone();
                path.push(scope.children.len());
                let own = own_intent(body);
                let mut child = RawScope::new(path);
                child.boxed = true;
                child.title = own.clone();
                child.intent = intent_chain(scope.intent.as_deref(), own.as_deref());
                child.declared = built.order;
                stack.push(built.layer);
                walk_forms(body, false, root_intent, stack, vocab, &mut child, legend);
                stack.pop();
                scope.children.push(child);
            }
        }
    }
}

fn walk_formula(
    formula: &Formula,
    negated: bool,
    root_intent: &Option<String>,
    stack: &mut Vec<Layer>,
    vocab: &Vocabulary,
    scope: &mut RawScope,
    legend: &mut LegendAcc,
) {
    match formula {
        Formula::And(xs) | Formula::Or(xs) => {
            for formula in xs {
                walk_formula(formula, negated, root_intent, stack, vocab, scope, legend);
            }
        }
        Formula::Not(inner) => {
            walk_formula(inner, !negated, root_intent, stack, vocab, scope, legend);
        }
        Formula::Apply { .. } => {
            if let Some(edge) =
                binary_edge(formula, negated, root_intent, stack, scope, vocab, legend)
            {
                scope.edges.push(edge);
            }
        }
        Formula::Claim { sentence, support } => {
            walk_formula(sentence, negated, root_intent, stack, vocab, scope, legend);
            walk_formula(support, negated, root_intent, stack, vocab, scope, legend);
        }
        Formula::Witness { .. } => {}
    }
}

/// Innermost scope that declares an endpoint name or the sense. Literals and
/// imports do not pin, so an edge they alone make is drawn at the root.
fn edge_home(
    edge_from: &End,
    edge_to: &End,
    sense: &Option<(String, String)>,
    stack: &[Layer],
    path: &[usize],
) -> Vec<usize> {
    let mut index = 0usize;
    for (at, layer) in stack.iter().enumerate() {
        let name_pins = [edge_from, edge_to].iter().any(|end| match end {
            End::Declared(name) => layer.names.contains(name),
            End::Imported(_) | End::Literal(_) => false,
        });
        let sense_pins = sense
            .as_ref()
            .is_some_and(|(name, _)| layer.senses.contains_key(name));
        if name_pins || sense_pins {
            index = at;
        }
    }
    path[..index].to_vec()
}

/// Move each edge onto the scope whose path is its home.
fn rehome(scope: &mut RawScope) -> Vec<RawEdge> {
    let mut upward = Vec::new();
    for child in &mut scope.children {
        upward.extend(rehome(child));
    }
    upward.extend(scope.edges.drain(..));
    let mut stay = Vec::new();
    let mut further = Vec::new();
    for edge in upward {
        if edge.home == scope.path {
            stay.push(edge);
        } else {
            further.push(edge);
        }
    }
    scope.edges = stay;
    further
}

fn visible_names(stack: &[Layer], vocab: &Vocabulary) -> HashSet<String> {
    let mut names = vocab.imported_names().clone();
    for layer in stack {
        names.extend(layer.names.iter().cloned());
    }
    names
}

fn visible_senses(stack: &[Layer], vocab: &Vocabulary) -> HashMap<String, Sense> {
    let mut senses = vocab.imported_senses().clone();
    for layer in stack {
        for (name, sense) in &layer.senses {
            senses.insert(name.clone(), sense.clone());
        }
    }
    senses
}

fn local_names(stack: &[Layer]) -> HashSet<String> {
    let mut names = HashSet::new();
    for layer in stack {
        names.extend(layer.names.iter().cloned());
    }
    names
}

fn local_senses(stack: &[Layer]) -> HashMap<String, Sense> {
    let mut senses = HashMap::new();
    for layer in stack {
        for (name, sense) in &layer.senses {
            senses.insert(name.clone(), sense.clone());
        }
    }
    senses
}

fn binary_edge(
    formula: &Formula,
    negated: bool,
    root_intent: &Option<String>,
    stack: &[Layer],
    scope: &RawScope,
    vocab: &Vocabulary,
    legend: &mut LegendAcc,
) -> Option<RawEdge> {
    let Formula::Apply { reading, args } = formula else {
        return None;
    };
    if args.len() != 2 {
        return None;
    }
    let from = endpoint(&args[0], stack, vocab)?;
    let to = endpoint(&args[1], stack, vocab)?;
    let (base, sense) = match reading {
        Reading::Sense(ident) => {
            let resolved = vocab.resolve_sense(ident, &local_senses(stack)).ok()?;
            if resolved.holes.len() != 2 {
                return None;
            }
            if !stack
                .iter()
                .any(|layer| layer.senses.contains_key(&resolved.name))
            {
                legend.push_sense(&resolved.name, &resolved.template, root_intent.clone());
            }
            (
                resolved.name.clone(),
                Some((resolved.name, resolved.template)),
            )
        }
        Reading::Inline { template, holes } => {
            if holes.len() != 2 {
                return None;
            }
            legend.push_reading(template);
            (template.clone(), None)
        }
    };
    let label = if negated { format!("not {base}") } else { base };
    let sentence = sentence(formula, stack, vocab)?;
    let home = edge_home(&from, &to, &sense, stack, &scope.path);
    Some(RawEdge {
        from,
        to,
        label,
        sentence,
        negated,
        sense,
        home,
    })
}

fn sentence(formula: &Formula, stack: &[Layer], vocab: &Vocabulary) -> Option<String> {
    let Formula::Apply { reading, args } = formula else {
        return None;
    };
    let mut spellings = Vec::with_capacity(args.len());
    for arg in args {
        spellings.push(spelling_of(arg, stack, vocab)?);
    }
    let template = match reading {
        Reading::Sense(ident) => {
            vocab
                .resolve_sense(ident, &local_senses(stack))
                .ok()?
                .template
        }
        Reading::Inline { template, .. } => template.clone(),
    };
    apply(&template, &spellings).ok()
}

fn spelling_of(arg: &Arg, stack: &[Layer], vocab: &Vocabulary) -> Option<String> {
    match arg {
        Arg::Literal(text) => Some(text.clone()),
        Arg::Name(ident) => vocab
            .resolve_name(ident, &local_names(stack))
            .ok()
            .map(|(spelling, _)| spelling),
    }
}

fn endpoint(arg: &Arg, stack: &[Layer], vocab: &Vocabulary) -> Option<End> {
    match arg {
        Arg::Literal(text) => {
            if stack.iter().any(|layer| layer.names.contains(text)) {
                Some(End::Declared(text.clone()))
            } else if vocab.imported_names().contains(text) {
                Some(End::Imported(text.clone()))
            } else {
                Some(End::Literal(text.clone()))
            }
        }
        Arg::Name(ident) => match vocab.resolve_name(ident, &local_names(stack)) {
            Ok((spelling, NameOrigin::Declared)) => Some(End::Declared(spelling)),
            Ok((spelling, NameOrigin::Imported)) => Some(End::Imported(spelling)),
            Err(_) => None,
        },
    }
}

struct IdGen {
    used: HashMap<String, usize>,
}

impl IdGen {
    fn fresh(&mut self, prefix: &str, label: &str) -> String {
        let mut raw = String::new();
        for c in label.chars() {
            if c.is_ascii_alphanumeric() {
                raw.push(c);
            } else if !raw.ends_with('_') {
                raw.push('_');
            }
        }
        let trimmed = raw.trim_matches('_');
        let body = if trimmed.is_empty() {
            "_".to_string()
        } else {
            trimmed.to_string()
        };
        let base = format!("{prefix}_{body}");
        let count = self.used.entry(base.clone()).or_insert(0);
        *count += 1;
        if *count == 1 {
            base
        } else {
            format!("{base}_{count}")
        }
    }
}

fn assemble(raw: RawScope, legend_rows: Vec<LegendDraft>) -> Diagram {
    let mut sites: HashMap<String, Vec<Vec<usize>>> = HashMap::new();
    let mut literal_order = Vec::new();
    let mut seen_literal = HashSet::new();
    let mut import_order = Vec::new();
    let mut seen_import = HashSet::new();
    collect(
        &raw,
        &mut sites,
        &mut literal_order,
        &mut seen_literal,
        &mut import_order,
        &mut seen_import,
    );
    let homes: HashMap<String, Vec<usize>> = sites
        .into_iter()
        .map(|(name, paths)| (name, outermost(&paths)))
        .collect();
    let mut ids = IdGen {
        used: HashMap::new(),
    };
    let mut declared_ids = HashMap::new();
    let mut imported_ids = HashMap::new();
    let mut literal_ids = HashMap::new();
    let root = place(
        &raw,
        &homes,
        &literal_order,
        &import_order,
        &mut ids,
        &mut declared_ids,
        &mut imported_ids,
        &mut literal_ids,
    );
    let legend = legend_rows
        .into_iter()
        .map(|row| {
            let basis = row.name.as_deref().unwrap_or(row.reading.as_str());
            LegendRow {
                id: ids.fresh("legend", basis),
                name: row.name,
                reading: row.reading,
                intent: row.intent,
            }
        })
        .collect();
    Diagram { root, legend }
}

fn collect(
    scope: &RawScope,
    sites: &mut HashMap<String, Vec<Vec<usize>>>,
    literal_order: &mut Vec<String>,
    seen_literal: &mut HashSet<String>,
    import_order: &mut Vec<String>,
    seen_import: &mut HashSet<String>,
) {
    for edge in &scope.edges {
        for end in [&edge.from, &edge.to] {
            match end {
                End::Imported(name) => {
                    if seen_import.insert(name.clone()) {
                        import_order.push(name.clone());
                    }
                }
                End::Literal(name) => {
                    sites
                        .entry(name.clone())
                        .or_default()
                        .push(scope.path.clone());
                    if seen_literal.insert(name.clone()) {
                        literal_order.push(name.clone());
                    }
                }
                End::Declared(_) => {}
            }
        }
    }
    for child in &scope.children {
        collect(
            child,
            sites,
            literal_order,
            seen_literal,
            import_order,
            seen_import,
        );
    }
}

/// The shallowest scope that contains every use of a spelling.
fn outermost(paths: &[Vec<usize>]) -> Vec<usize> {
    let Some(first) = paths.first() else {
        return Vec::new();
    };
    let mut prefix = first.clone();
    for path in &paths[1..] {
        let shared = prefix
            .iter()
            .zip(path.iter())
            .take_while(|(left, right)| left == right)
            .count();
        prefix.truncate(shared);
    }
    prefix
}

fn place(
    raw: &RawScope,
    homes: &HashMap<String, Vec<usize>>,
    literal_order: &[String],
    import_order: &[String],
    ids: &mut IdGen,
    declared_ids: &mut HashMap<(Vec<usize>, String), String>,
    imported_ids: &mut HashMap<String, String>,
    literal_ids: &mut HashMap<String, String>,
) -> Scope {
    let mut nodes = Vec::new();
    for name in &raw.declared {
        let id = ids.fresh("n", name);
        declared_ids.insert((raw.path.clone(), name.clone()), id.clone());
        nodes.push(Node {
            id,
            name: name.clone(),
            intent: raw.intent.clone(),
        });
    }
    if raw.path.is_empty() {
        for name in import_order {
            let id = ids.fresh("n", name);
            imported_ids.insert(name.clone(), id.clone());
            nodes.push(Node {
                id,
                name: name.clone(),
                intent: raw.intent.clone(),
            });
        }
    }
    for name in literal_order {
        if homes.get(name) == Some(&raw.path) {
            let id = ids.fresh("n", name);
            literal_ids.insert(name.clone(), id.clone());
            nodes.push(Node {
                id,
                name: name.clone(),
                intent: raw.intent.clone(),
            });
        }
    }
    // A shared id would draw an edge from this subgraph back to the scope that
    // declared the name. A copy keeps the detail's edges inside it.
    let mut copies: HashMap<String, String> = HashMap::new();
    let mut edges = Vec::new();
    for edge in &raw.edges {
        let Some(from) = node_for(
            &edge.from,
            &raw.path,
            homes,
            ids,
            &mut nodes,
            &mut copies,
            declared_ids,
            imported_ids,
            literal_ids,
        ) else {
            continue;
        };
        let Some(to) = node_for(
            &edge.to,
            &raw.path,
            homes,
            ids,
            &mut nodes,
            &mut copies,
            declared_ids,
            imported_ids,
            literal_ids,
        ) else {
            continue;
        };
        edges.push(Edge {
            from,
            to,
            label: edge.label.clone(),
            sentence: edge.sentence.clone(),
            negated: edge.negated,
            contradicted: false,
            sense: edge.sense.clone(),
        });
    }
    let children = raw
        .children
        .iter()
        .filter_map(|child| {
            let placed = place(
                child,
                homes,
                literal_order,
                import_order,
                ids,
                declared_ids,
                imported_ids,
                literal_ids,
            );
            kept(placed)
        })
        .collect();
    Scope {
        boxed: raw.boxed,
        title: raw.title.clone(),
        nodes,
        edges,
        children,
    }
}

/// A detail with nothing drawn is not a box. The root is kept.
fn kept(scope: Scope) -> Option<Scope> {
    if scope.boxed && scope.nodes.is_empty() && scope.edges.is_empty() && scope.children.is_empty()
    {
        None
    } else {
        Some(scope)
    }
}

fn node_for(
    end: &End,
    path: &[usize],
    homes: &HashMap<String, Vec<usize>>,
    ids: &mut IdGen,
    nodes: &mut Vec<Node>,
    copies: &mut HashMap<String, String>,
    declared_ids: &HashMap<(Vec<usize>, String), String>,
    imported_ids: &HashMap<String, String>,
    literal_ids: &HashMap<String, String>,
) -> Option<String> {
    if let Some(id) = local_node(end, path, homes, declared_ids, imported_ids, literal_ids) {
        return Some(id);
    }
    if path.is_empty() {
        return None;
    }
    let name = match end {
        End::Declared(name) | End::Imported(name) | End::Literal(name) => name,
    };
    if let Some(id) = copies.get(name) {
        return Some(id.clone());
    }
    let id = ids.fresh("n", name);
    copies.insert(name.clone(), id.clone());
    nodes.push(Node {
        id: id.clone(),
        name: name.clone(),
        intent: None,
    });
    Some(id)
}

/// The node this scope itself declares. An outer name is absent here.
fn local_node(
    end: &End,
    path: &[usize],
    homes: &HashMap<String, Vec<usize>>,
    declared_ids: &HashMap<(Vec<usize>, String), String>,
    imported_ids: &HashMap<String, String>,
    literal_ids: &HashMap<String, String>,
) -> Option<String> {
    match end {
        End::Declared(name) => declared_ids.get(&(path.to_vec(), name.clone())).cloned(),
        End::Imported(name) if path.is_empty() => imported_ids.get(name).cloned(),
        End::Literal(name) if homes.get(name).map(Vec::as_slice) == Some(path) => {
            literal_ids.get(name).cloned()
        }
        End::Imported(_) | End::Literal(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::{link_parts, Loaded};
    use crate::syntax::parse;

    fn drawn(src: &str) -> String {
        let program = parse(src).unwrap();
        render(&diagram_of(&program, &Vocabulary::none()), &Styles::plain())
    }

    fn answer(choice: &str) -> Result<Answer, OracleError> {
        Ok(Answer {
            choice: choice.to_string(),
            confidence: 0.4,
            noul: None,
        })
    }

    #[test]
    fn diagram_sits_beside_the_trace() {
        assert_eq!(
            diagram_path(Path::new("/tmp/autoground/1790-51022-0/trace.jsonl")),
            PathBuf::from("/tmp/autoground/1790-51022-0/diagram.mmd")
        );
    }

    #[test]
    fn quoted_shapes_keep_the_label_inside_the_brackets() {
        assert_eq!(shaped("n", "Frontend", Shape::Rectangle), "n[\"Frontend\"]");
        assert_eq!(shaped("n", "Frontend", Shape::Rounded), "n(\"Frontend\")");
        assert_eq!(
            shaped("n", "Frontend", Shape::Cylinder),
            "n[(\"Frontend\")]"
        );
    }

    #[test]
    fn a_diagram_brief_draws_binary_senses_and_skips_prose() {
        let text = drawn(include_str!("../../examples/diagram.brief"));
        assert!(text.contains("n_Frontend[\"Frontend\"]:::normal"), "{text}");
        assert!(text.contains("n_Backend[\"Backend\"]:::normal"), "{text}");
        assert!(text.contains("n_Oracle[\"Oracle\"]:::normal"), "{text}");
        assert!(text.contains("n_TypeSafe[\"TypeSafe\"]:::normal"), "{text}");
        assert!(
            text.contains("n_Frontend -->|\"depends_on\"| n_Backend"),
            "{text}"
        );
        assert!(
            text.contains("n_Backend -->|\"not depends_on\"| n_Frontend"),
            "{text}"
        );
        assert!(
            text.contains("n_Oracle -->|\"calls\"| n_TypeSafe"),
            "{text}"
        );
        assert!(
            text.contains("n_Frontend -->|\"{caller} calls {callee}\"| n_Backend"),
            "{text}"
        );
        assert!(!text.contains("component of the system"), "{text}");
        assert!(!text.contains("TypeSafe API"), "{text}");
        assert!(
            text.contains("legend_depends_on[\"depends_on — {X} depends on {Y}\"]"),
            "{text}"
        );
        assert!(
            text.contains("legend_calls[\"calls — {caller} calls {callee}\"]"),
            "{text}"
        );
        assert!(
            text.contains("legend_caller_calls_callee[\"{caller} calls {callee}\"]"),
            "{text}"
        );
    }

    #[test]
    fn a_double_not_draws_the_sense_plain() {
        let text = drawn(
            r#"
(name A)
(name B)
(sense calls "{X} calls {Y}")
(not (not (calls A B)))
"#,
        );
        assert!(text.contains("n_A -->|\"calls\"| n_B"), "{text}");
        assert!(!text.contains("not calls"), "{text}");
    }

    #[test]
    fn a_literal_argument_is_a_node() {
        let text = drawn(
            r#"
(name Universe)
(sense source "{component} is implemented at {path}")
(source Universe "src/universe")
"#,
        );
        assert!(
            text.contains("n_Universe -->|\"source\"| n_src_universe"),
            "{text}"
        );
        assert!(
            text.contains("n_src_universe[\"src/universe\"]:::normal"),
            "{text}"
        );
    }

    #[test]
    fn a_detail_copies_an_outer_name_instead_of_linking_to_it() {
        assert_eq!(
            drawn(
                r#"
(name Concept)
(detail
  (intent "inputs")
  (name LogicBlock)
  (name Relationship)
  (sense input "{X} is an input into {Y}")
  (input Concept LogicBlock)
  (input Relationship LogicBlock))
"#,
            ),
            "\
flowchart TD
  classDef deemphasized fill:#f4f4f5,stroke:#a1a1aa,stroke-width:1px,stroke-dasharray: 4 3
  classDef normal fill:#f8fafc,stroke:#334155,stroke-width:1.5px
  classDef highlighted fill:#fef3c7,stroke:#b45309,stroke-width:2.5px
  n_Concept[\"Concept\"]:::normal
  subgraph d1 [\"inputs\"]
    n_LogicBlock[\"LogicBlock\"]:::normal
    n_Relationship[\"Relationship\"]:::normal
    n_Concept_2[\"Concept\"]:::normal
    n_Concept_2 -->|\"input\"| n_LogicBlock
    n_Relationship -->|\"input\"| n_LogicBlock
  end
  linkStyle 0 stroke:#334155,stroke-width:1.5px
  linkStyle 1 stroke:#334155,stroke-width:1.5px
  subgraph senses [\"senses\"]
    legend_input[\"input — {X} is an input into {Y}\"]
  end
"
        );
    }

    #[test]
    fn an_edge_of_outer_names_lifts_out_of_a_detail() {
        let text = drawn(
            r#"
(name Relationship)
(name LogicBlock)
(sense input "{X} is an input into {Y}")
(detail
  (intent "note")
  (name Scratch)
  (input Relationship LogicBlock))
"#,
        );
        let subgraph = text
            .split("subgraph d1")
            .nth(1)
            .and_then(|rest| rest.split("end").next())
            .unwrap_or("");
        assert!(
            subgraph.contains("n_Scratch[\"Scratch\"]:::normal"),
            "{text}"
        );
        assert!(!subgraph.contains("-->"), "{text}");
        assert!(
            text.contains("n_Relationship -->|\"input\"| n_LogicBlock"),
            "{text}"
        );
        assert!(text.contains("subgraph d1 [\"note\"]"), "{text}");
    }

    #[test]
    fn a_local_sense_keeps_an_edge_inside_the_detail() {
        let text = drawn(
            r#"
(name Relationship)
(name LogicBlock)
(detail
  (intent "inputs")
  (sense input "{X} is an input into {Y}")
  (input Relationship LogicBlock))
"#,
        );
        let subgraph = text
            .split("subgraph d1")
            .nth(1)
            .and_then(|rest| rest.split("end").next())
            .unwrap_or("");
        assert!(subgraph.contains("-->|\"input\"|"), "{text}");
        let outside = text.split("subgraph d1").next().unwrap_or("");
        assert!(!outside.contains("-->"), "{text}");
    }

    #[test]
    fn sibling_details_keep_distinct_ids_for_one_spelling() {
        let text = drawn(
            r#"
(detail
  (name Local)
  ("Local holds"))
(detail
  (name Local)
  ("Local holds"))
"#,
        );
        assert!(text.contains("subgraph d1\n"), "{text}");
        assert!(text.contains("subgraph d2\n"), "{text}");
        assert!(!text.contains("subgraph d1 ["), "{text}");
        assert!(text.contains("n_Local[\"Local\"]:::normal"), "{text}");
        assert!(text.contains("n_Local_2[\"Local\"]:::normal"), "{text}");
        assert!(!text.contains("Local holds"), "{text}");
    }

    #[test]
    fn priority_picks_the_cylinder_the_arrow_and_the_link_style() {
        let program = parse(include_str!("../../examples/diagram.brief")).unwrap();
        let diagram = diagram_of(&program, &Vocabulary::none());
        let mut styles = Styles::plain();
        styles.kinds.insert("Frontend".into(), Kind::Data);
        styles
            .name_priority
            .insert("Frontend".into(), Priority::Highlighted);
        styles.sense_priority.insert(
            ("calls".into(), "{caller} calls {callee}".into()),
            Priority::Deemphasized,
        );
        let text = render(&diagram, &styles);
        assert!(
            text.contains("n_Frontend[(\"Frontend\")]:::highlighted"),
            "{text}"
        );
        assert!(
            text.contains("n_Oracle -.->|\"calls\"| n_TypeSafe"),
            "{text}"
        );
        assert!(
            text.contains("linkStyle 2 stroke:#a1a1aa,stroke-width:1px"),
            "{text}"
        );
        assert!(
            text.contains("linkStyle 0 stroke:#334155,stroke-width:1.5px"),
            "{text}"
        );
    }

    #[test]
    fn a_component_is_rounded_and_data_is_a_cylinder() {
        let program = parse("(name Frontend) (name Catalog)").unwrap();
        let diagram = diagram_of(&program, &Vocabulary::none());
        let batch = style_questions(&diagram, "").unwrap();
        let answers = vec![
            answer("component"),
            answer("normal"),
            answer("data"),
            answer("normal"),
        ];
        let text = render(
            &diagram,
            &from_answers(&batch.names, &batch.senses, &answers),
        );
        assert!(text.contains("n_Frontend(\"Frontend\"):::normal"), "{text}");
        assert!(text.contains("n_Catalog[(\"Catalog\")]:::normal"), "{text}");
    }

    #[test]
    fn a_contradicted_sentence_dashes_its_edge_in_red() {
        let mut diagram = diagram_of(
            &parse(include_str!("../../examples/diagram.brief")).unwrap(),
            &Vocabulary::none(),
        );
        diagram.mark_contradicted(&HashSet::from([
            "Frontend depends on Backend".into(),
            "Backend depends on Frontend".into(),
        ]));
        let text = render(&diagram, &Styles::plain());
        assert!(
            text.contains("n_Frontend -.->|\"depends_on\"| n_Backend"),
            "{text}"
        );
        assert!(
            text.contains("linkStyle 0 stroke:#dc2626,stroke-width:1.5px,stroke-dasharray: 4 3"),
            "{text}"
        );
        assert!(
            text.contains("n_Backend -->|\"not depends_on\"| n_Frontend"),
            "{text}"
        );
    }

    #[test]
    fn an_unrecognized_choice_stays_a_normal_rectangle() {
        let program = parse("(name Frontend)").unwrap();
        let diagram = diagram_of(&program, &Vocabulary::none());
        let batch = style_questions(&diagram, "").unwrap();
        let answers = vec![answer("triangle"), answer("loud")];
        let styles = from_answers(&batch.names, &batch.senses, &answers);
        let text = render(&diagram, &styles);
        assert!(text.contains("n_Frontend[\"Frontend\"]:::normal"), "{text}");
    }

    #[test]
    fn the_style_state_is_the_verbalized_context() {
        let context = "\
Relationship is an input into LogicBlock
(see reasoners/prescriptive_reasoner.py)

LogicBlockBuilder produces LogicBlock
(see reasoners/logic_reasoner.py)
";
        let program = parse("(name LogicBlock) (name LogicBlockBuilder)").unwrap();
        let diagram = diagram_of(&program, &Vocabulary::none());
        let batch = style_questions(&diagram, context).unwrap();
        let state = batch.questions[0].state.json();
        assert_eq!(state, &json!(context));
        assert!(batch
            .questions
            .iter()
            .all(|question| question.state.json() == state));
    }

    #[test]
    fn style_questions_share_the_context_and_ask_per_name() {
        let program = parse(include_str!("../../examples/diagram.brief")).unwrap();
        let diagram = diagram_of(&program, &Vocabulary::none());
        let context = "Frontend depends on Backend\n";
        let batch = style_questions(&diagram, context).unwrap();
        assert_eq!(
            batch.names,
            vec!["Frontend", "Backend", "Oracle", "TypeSafe"]
        );
        assert_eq!(
            batch.senses,
            vec![
                ("depends_on".into(), "{X} depends on {Y}".into()),
                ("calls".into(), "{caller} calls {callee}".into()),
            ]
        );
        assert_eq!(batch.questions.len(), 2 * 4 + 2);
        let state = batch.questions[0].state.json();
        assert_eq!(state, &json!(context));
        assert_eq!(
            batch.questions[0].instructions["question"],
            "Is this name a component or data?"
        );
        let kind_keys: HashSet<_> = batch.questions[0]
            .criteria
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            kind_keys,
            HashSet::from(["component".into(), "data".into()])
        );
        let priority_keys: HashSet<_> = batch.questions[1]
            .criteria
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            priority_keys,
            HashSet::from(["deemphasized".into(), "normal".into(), "highlighted".into(),])
        );
        assert_eq!(
            batch.questions[8].instructions["question"],
            "How strongly should this relation read?"
        );
        assert_eq!(batch.questions[8].instructions["name"], "depends_on");
        assert!(batch
            .questions
            .iter()
            .all(|question| question.state.json() == state));
    }

    #[test]
    fn intent_is_the_declaring_scope_in_the_style_state() {
        let program = parse(
            r#"
(intent "the picture")
(name Outer)
(sense calls "{X} calls {Y}")
(detail
  (name Inner)
  (calls Outer Inner))
(detail
  (intent "the step")
  (name Local)
  (sense feeds "{X} feeds {Y}")
  (feeds Local Outer))
"#,
        )
        .unwrap();
        let diagram = diagram_of(&program, &Vocabulary::none());
        let batch = style_questions(&diagram, "green\n").unwrap();
        assert_eq!(batch.names, vec!["Outer", "Inner", "Local"]);
        let picture = json!({ "context": "green\n", "intent": "the picture" });
        let step = json!({ "context": "green\n", "intent": "the picture\nthe step" });
        assert_eq!(batch.questions[0].state.json(), &picture);
        assert_eq!(batch.questions[0].instructions["name"], "Outer");
        assert_eq!(batch.questions[2].state.json(), &picture);
        assert_eq!(batch.questions[2].instructions["name"], "Inner");
        assert_eq!(batch.questions[4].state.json(), &step);
        assert_eq!(batch.questions[4].instructions["name"], "Local");
        let feeds = batch
            .questions
            .iter()
            .find(|question| question.instructions["name"] == "feeds")
            .unwrap();
        assert_eq!(feeds.state.json(), &step);
        let calls = batch
            .questions
            .iter()
            .find(|question| question.instructions["name"] == "calls")
            .unwrap();
        assert_eq!(calls.state.json(), &picture);
    }

    #[test]
    fn a_prose_brief_is_only_the_legend_frame() {
        assert_eq!(
            drawn("(\"there is a GPU compiler\")"),
            "\
flowchart TD
  classDef deemphasized fill:#f4f4f5,stroke:#a1a1aa,stroke-width:1px,stroke-dasharray: 4 3
  classDef normal fill:#f8fafc,stroke:#334155,stroke-width:1.5px
  classDef highlighted fill:#fef3c7,stroke:#b45309,stroke-width:2.5px
  subgraph senses [\"senses\"]
  end
"
        );
        let program = parse("(\"there is a GPU compiler\")").unwrap();
        assert!(style_questions(&diagram_of(&program, &Vocabulary::none()), "").is_none());
    }

    #[test]
    fn an_imported_sense_is_verbalized_when_an_edge_uses_it() {
        let files = vec![Loaded {
            path: "product.brief".into(),
            program: parse(
                r#"
(ns com.acme.product)
(name Frontend)
(sense part_of "{X} is part of {Y}")
"#,
            )
            .unwrap(),
        }];
        let program = parse(
            r#"
(ns com.acme.client
  (:require [com.acme.product :refer [Frontend part_of]]))
(name Backend)
(part_of Frontend Backend)
"#,
        )
        .unwrap();
        let (_compiled, vocab) = link_parts(&program, Some("<input>"), &files).unwrap();
        let text = render(&diagram_of(&program, &vocab), &Styles::plain());
        assert!(text.contains("n_Frontend[\"Frontend\"]:::normal"), "{text}");
        assert!(text.contains("n_Backend[\"Backend\"]:::normal"), "{text}");
        assert!(
            text.contains("n_Frontend -->|\"part_of\"| n_Backend"),
            "{text}"
        );
        assert!(
            text.contains("legend_part_of[\"part_of — {X} is part of {Y}\"]"),
            "{text}"
        );
    }
}
