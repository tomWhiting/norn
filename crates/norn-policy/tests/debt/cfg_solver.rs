use norn_policy::debt::DebtConstructKind;

use super::support::{TestResult, scan};

const MAX_EXHAUSTIVE_NODES: usize = 4;

#[derive(Clone, Copy)]
enum OracleAtom {
    First,
    Second,
}

#[derive(Clone)]
enum OracleFormula {
    Atom(OracleAtom),
    True,
    False,
    Not(Box<Self>),
    All(Box<Self>, Box<Self>),
    Any(Box<Self>, Box<Self>),
}

impl OracleFormula {
    fn render(&self) -> String {
        match self {
            Self::Atom(OracleAtom::First) => "atom_0".to_owned(),
            Self::Atom(OracleAtom::Second) => "atom_1".to_owned(),
            Self::True => "all()".to_owned(),
            Self::False => "any()".to_owned(),
            Self::Not(part) => format!("not({})", part.render()),
            Self::All(left, right) => format!("all({}, {})", left.render(), right.render()),
            Self::Any(left, right) => format!("any({}, {})", left.render(), right.render()),
        }
    }

    fn evaluate(&self, assignment: [bool; 2]) -> bool {
        match self {
            Self::Atom(OracleAtom::First) => assignment[0],
            Self::Atom(OracleAtom::Second) => assignment[1],
            Self::True => true,
            Self::False => false,
            Self::Not(part) => !part.evaluate(assignment),
            Self::All(left, right) => left.evaluate(assignment) && right.evaluate(assignment),
            Self::Any(left, right) => left.evaluate(assignment) || right.evaluate(assignment),
        }
    }

    fn truth_table_satisfiable(&self) -> bool {
        (0..4).any(|bits| {
            let assignment = [bits & 1 != 0, bits & 2 != 0];
            self.evaluate(assignment)
        })
    }
}

#[test]
fn exact_solver_matches_exhaustive_small_truth_tables() -> TestResult {
    let formulas = formulas_by_node_count(MAX_EXHAUSTIVE_NODES);
    for formula in formulas.into_iter().flatten() {
        let rendered = formula.render();
        let source = format!("#[cfg({rendered})]\nfn guarded() {{}}\n");
        let occurrences = scan(&source)?;
        let reported_impossible = occurrences
            .iter()
            .any(|occurrence| occurrence.construct() == DebtConstructKind::ImpossibleCfg);
        assert_eq!(
            reported_impossible,
            !formula.truth_table_satisfiable(),
            "truth-table disagreement for {rendered}"
        );
    }
    Ok(())
}

#[test]
fn irrelevant_tautology_prefix_does_not_force_assignment_enumeration() -> TestResult {
    let mut parts: Vec<String> = (0..64)
        .map(|index| format!("any(padding_{index}, not(padding_{index}))"))
        .collect();
    parts.push("terminal".to_owned());
    parts.push("not(terminal)".to_owned());
    let source = format!("#[cfg(all({}))]\nfn guarded() {{}}\n", parts.join(", "));

    let occurrences = scan(&source)?;
    assert_eq!(occurrences.len(), 1);
    assert_eq!(occurrences[0].construct(), DebtConstructKind::ImpossibleCfg);
    Ok(())
}

#[test]
fn empty_all_is_true_and_empty_any_is_false() -> TestResult {
    let true_occurrences = scan("#[cfg(all())]\nfn enabled() {}\n")?;
    assert!(true_occurrences.is_empty());

    let false_occurrences = scan("#[cfg(any())]\nfn disabled() {}\n")?;
    assert_eq!(false_occurrences.len(), 1);
    assert_eq!(
        false_occurrences[0].construct(),
        DebtConstructKind::ImpossibleCfg
    );
    Ok(())
}

#[test]
fn malformed_formula_still_fails_closed() {
    let result = scan("#[cfg(not(unix, windows))]\nfn guarded() {}\n");
    assert!(result.is_err());
}

fn formulas_by_node_count(max_nodes: usize) -> Vec<Vec<OracleFormula>> {
    let mut formulas = vec![Vec::new(); max_nodes + 1];
    formulas[1] = vec![
        OracleFormula::Atom(OracleAtom::First),
        OracleFormula::Atom(OracleAtom::Second),
        OracleFormula::True,
        OracleFormula::False,
    ];

    for nodes in 2..=max_nodes {
        let mut exact = formulas[nodes - 1]
            .iter()
            .cloned()
            .map(Box::new)
            .map(OracleFormula::Not)
            .collect::<Vec<_>>();
        for left_nodes in 1..nodes - 1 {
            let right_nodes = nodes - 1 - left_nodes;
            for left in &formulas[left_nodes] {
                for right in &formulas[right_nodes] {
                    exact.push(OracleFormula::All(
                        Box::new(left.clone()),
                        Box::new(right.clone()),
                    ));
                    exact.push(OracleFormula::Any(
                        Box::new(left.clone()),
                        Box::new(right.clone()),
                    ));
                }
            }
        }
        formulas[nodes] = exact;
    }
    formulas
}
