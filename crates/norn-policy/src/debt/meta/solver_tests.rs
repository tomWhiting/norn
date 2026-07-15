//! Differential and branch-coverage tests for the iterative DPLL engine.

use super::{Cnf, Literal, SolveOutcome, solve, validates_complete_model};
use crate::debt::model::DebtScanError;

#[test]
fn unit_clause_propagates_without_a_decision() -> Result<(), DebtScanError> {
    let cnf = Cnf::new(1, vec![vec![Literal::positive(0)]]);
    assert_eq!(solve(&cnf)?, SolveOutcome::Satisfiable(vec![true]));
    Ok(())
}

#[test]
fn contradictory_units_close_the_root_branch() -> Result<(), DebtScanError> {
    let cnf = Cnf::new(
        1,
        vec![vec![Literal::positive(0)], vec![Literal::negative(0)]],
    );
    assert_eq!(solve(&cnf)?, SolveOutcome::Unsatisfiable);
    Ok(())
}

#[test]
fn tautology_accepts_the_deterministic_first_branch() -> Result<(), DebtScanError> {
    let cnf = Cnf::new(1, vec![vec![Literal::positive(0), Literal::negative(0)]]);
    assert_eq!(solve(&cnf)?, SolveOutcome::Satisfiable(vec![false]));
    Ok(())
}

#[test]
fn first_decision_branch_can_produce_a_complete_model() -> Result<(), DebtScanError> {
    let cnf = Cnf::new(2, vec![vec![Literal::negative(0), Literal::positive(1)]]);
    assert_eq!(solve(&cnf)?, SolveOutcome::Satisfiable(vec![false, false]));
    Ok(())
}

#[test]
fn conflict_retries_the_second_decision_branch() -> Result<(), DebtScanError> {
    let cnf = Cnf::new(
        2,
        vec![
            vec![Literal::positive(0), Literal::positive(1)],
            vec![Literal::positive(0), Literal::negative(1)],
        ],
    );
    assert_eq!(solve(&cnf)?, SolveOutcome::Satisfiable(vec![true, false]));
    Ok(())
}

#[test]
fn conflict_exhausts_a_nested_decision_before_backtracking() -> Result<(), DebtScanError> {
    let cnf = Cnf::new(
        3,
        vec![
            vec![
                Literal::positive(0),
                Literal::positive(1),
                Literal::positive(2),
            ],
            vec![
                Literal::positive(0),
                Literal::positive(1),
                Literal::negative(2),
            ],
            vec![
                Literal::positive(0),
                Literal::negative(1),
                Literal::positive(2),
            ],
            vec![
                Literal::positive(0),
                Literal::negative(1),
                Literal::negative(2),
            ],
        ],
    );
    assert_eq!(
        solve(&cnf)?,
        SolveOutcome::Satisfiable(vec![true, false, false])
    );
    Ok(())
}

#[test]
fn empty_clause_is_unsatisfiable_and_empty_cnf_is_satisfiable() -> Result<(), DebtScanError> {
    assert_eq!(
        solve(&Cnf::new(0, vec![Vec::new()]))?,
        SolveOutcome::Unsatisfiable
    );
    assert_eq!(
        solve(&Cnf::new(0, Vec::new()))?,
        SolveOutcome::Satisfiable(Vec::new())
    );
    Ok(())
}

#[test]
fn invalid_variable_reference_fails_closed() {
    let cnf = Cnf::new(1, vec![vec![Literal::positive(1)]]);
    assert!(solve(&cnf).is_err());
}

#[test]
fn complete_model_validator_rejects_corruption() {
    let cnf = Cnf::new(
        2,
        vec![
            vec![Literal::positive(0), Literal::positive(1)],
            vec![Literal::negative(0), Literal::negative(1)],
        ],
    );
    assert!(validates_complete_model(&cnf, &[true, false]));
    assert!(!validates_complete_model(&cnf, &[true]));
    assert!(!validates_complete_model(&cnf, &[true, true]));
    assert!(!validates_complete_model(&cnf, &[false, false]));

    let invalid = Cnf::new(1, vec![vec![Literal::positive(1)]]);
    assert!(!validates_complete_model(&invalid, &[true]));
}

#[test]
fn solver_matches_every_two_variable_cnf_truth_table() -> Result<(), DebtScanError> {
    let possible_clauses = every_two_variable_clause();
    let cnf_count = 1usize << possible_clauses.len();
    for clause_mask in 0..cnf_count {
        let clauses = possible_clauses
            .iter()
            .enumerate()
            .filter(|(index, _)| clause_mask & (1usize << index) != 0)
            .map(|(_, clause)| clause.clone())
            .collect();
        let cnf = Cnf::new(2, clauses);
        let solver_satisfiable = matches!(solve(&cnf)?, SolveOutcome::Satisfiable(_));
        assert_eq!(
            solver_satisfiable,
            truth_table_satisfiable(&cnf),
            "truth-table disagreement for clause mask {clause_mask}"
        );
    }
    Ok(())
}

fn every_two_variable_clause() -> Vec<Vec<Literal>> {
    (0..16)
        .map(|literal_mask| {
            (0..4)
                .filter(|literal_index| literal_mask & (1usize << literal_index) != 0)
                .map(|literal_index| Literal {
                    variable: literal_index / 2,
                    polarity: literal_index % 2 != 0,
                })
                .collect()
        })
        .collect()
}

fn truth_table_satisfiable(cnf: &Cnf) -> bool {
    (0..4).any(|assignment| {
        let model = [assignment & 1 != 0, assignment & 2 != 0];
        validates_complete_model(cnf, &model)
    })
}
