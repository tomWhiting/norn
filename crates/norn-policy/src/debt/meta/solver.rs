//! Safe, exact, iterative satisfiability for typed CNF clauses.

use crate::debt::model::DebtScanError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Literal {
    pub(super) variable: usize,
    pub(super) polarity: bool,
}

impl Literal {
    pub(super) const fn positive(variable: usize) -> Self {
        Self {
            variable,
            polarity: true,
        }
    }

    pub(super) const fn negative(variable: usize) -> Self {
        Self {
            variable,
            polarity: false,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct Cnf {
    pub(super) variable_count: usize,
    pub(super) clauses: Vec<Vec<Literal>>,
}

impl Cnf {
    pub(super) const fn new(variable_count: usize, clauses: Vec<Vec<Literal>>) -> Self {
        Self {
            variable_count,
            clauses,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum SolveOutcome {
    Satisfiable(Vec<bool>),
    Unsatisfiable,
}

pub(super) fn solve(cnf: &Cnf) -> Result<SolveOutcome, DebtScanError> {
    let clause_index = ClauseIndex::build(cnf)?;
    let mut state = SearchState::new(cnf.variable_count);
    if !seed_unit_clauses(cnf, &mut state)? {
        return Ok(SolveOutcome::Unsatisfiable);
    }

    loop {
        if !propagate(cnf, &clause_index, &mut state)? {
            // Every frame tries false and then true. An empty frame stack here
            // means unit propagation closed the complete finite branch space.
            if state.retry_decision()? {
                continue;
            }
            return Ok(SolveOutcome::Unsatisfiable);
        }

        if let Some(variable) = state.first_unassigned() {
            state.start_decision(variable)?;
            continue;
        }

        let model = state.complete_model()?;
        if validates_complete_model(cnf, &model) {
            return Ok(SolveOutcome::Satisfiable(model));
        }
        return Err(DebtScanError::CfgSatisfiability);
    }
}

pub(super) fn validates_complete_model(cnf: &Cnf, model: &[bool]) -> bool {
    model.len() == cnf.variable_count
        && cnf.clauses.iter().all(|clause| {
            clause.iter().any(|literal| {
                model
                    .get(literal.variable)
                    .is_some_and(|value| *value == literal.polarity)
            })
        })
}

struct ClauseIndex {
    occurrences_by_polarity: Vec<[Vec<usize>; 2]>,
}

impl ClauseIndex {
    fn build(cnf: &Cnf) -> Result<Self, DebtScanError> {
        let mut occurrences_by_polarity = std::iter::repeat_with(|| [Vec::new(), Vec::new()])
            .take(cnf.variable_count)
            .collect::<Vec<_>>();
        for (clause_index, clause) in cnf.clauses.iter().enumerate() {
            for literal in clause {
                let Some(occurrences) = occurrences_by_polarity.get_mut(literal.variable) else {
                    return Err(DebtScanError::CfgSatisfiability);
                };
                occurrences[usize::from(literal.polarity)].push(clause_index);
            }
        }
        Ok(Self {
            occurrences_by_polarity,
        })
    }

    fn clauses_falsified_by(
        &self,
        variable: usize,
        value: bool,
    ) -> Result<&[usize], DebtScanError> {
        let Some(occurrences) = self.occurrences_by_polarity.get(variable) else {
            return Err(DebtScanError::CfgSatisfiability);
        };
        Ok(&occurrences[usize::from(!value)])
    }
}

#[derive(Clone, Copy)]
struct Decision {
    variable: usize,
    trail_checkpoint: usize,
    positive_tried: bool,
}

struct SearchState {
    values: Vec<Option<bool>>,
    trail: Vec<usize>,
    propagation_head: usize,
    decisions: Vec<Decision>,
}

impl SearchState {
    fn new(variable_count: usize) -> Self {
        Self {
            values: vec![None; variable_count],
            trail: Vec::new(),
            propagation_head: 0,
            decisions: Vec::new(),
        }
    }

    fn assign(&mut self, variable: usize, value: bool) -> Result<Assignment, DebtScanError> {
        let Some(slot) = self.values.get_mut(variable) else {
            return Err(DebtScanError::CfgSatisfiability);
        };
        match *slot {
            Some(current) if current == value => Ok(Assignment::Unchanged),
            Some(_) => Ok(Assignment::Conflict),
            None => {
                *slot = Some(value);
                self.trail.push(variable);
                Ok(Assignment::Assigned)
            }
        }
    }

    fn first_unassigned(&self) -> Option<usize> {
        self.values.iter().position(Option::is_none)
    }

    fn start_decision(&mut self, variable: usize) -> Result<(), DebtScanError> {
        let decision = Decision {
            variable,
            trail_checkpoint: self.trail.len(),
            positive_tried: false,
        };
        self.decisions.push(decision);
        if self.assign(variable, false)? == Assignment::Assigned {
            Ok(())
        } else {
            Err(DebtScanError::CfgSatisfiability)
        }
    }

    fn retry_decision(&mut self) -> Result<bool, DebtScanError> {
        loop {
            let Some(decision) = self.decisions.last().copied() else {
                return Ok(false);
            };
            self.rollback(decision.trail_checkpoint)?;
            if !decision.positive_tried {
                let Some(active) = self.decisions.last_mut() else {
                    return Err(DebtScanError::CfgSatisfiability);
                };
                active.positive_tried = true;
                return match self.assign(decision.variable, true)? {
                    Assignment::Assigned => Ok(true),
                    Assignment::Unchanged | Assignment::Conflict => {
                        Err(DebtScanError::CfgSatisfiability)
                    }
                };
            }
            self.decisions.pop();
        }
    }

    fn rollback(&mut self, checkpoint: usize) -> Result<(), DebtScanError> {
        while self.trail.len() > checkpoint {
            let Some(variable) = self.trail.pop() else {
                return Err(DebtScanError::CfgSatisfiability);
            };
            let Some(slot) = self.values.get_mut(variable) else {
                return Err(DebtScanError::CfgSatisfiability);
            };
            *slot = None;
        }
        if self.trail.len() != checkpoint {
            return Err(DebtScanError::CfgSatisfiability);
        }
        self.propagation_head = self.trail.len();
        Ok(())
    }

    fn complete_model(&self) -> Result<Vec<bool>, DebtScanError> {
        self.values
            .iter()
            .copied()
            .map(|value| value.ok_or(DebtScanError::CfgSatisfiability))
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Assignment {
    Assigned,
    Unchanged,
    Conflict,
}

#[derive(Clone, Copy)]
enum ClauseState {
    Satisfied,
    Unit(Literal),
    Unresolved,
    Conflict,
}

fn seed_unit_clauses(cnf: &Cnf, state: &mut SearchState) -> Result<bool, DebtScanError> {
    for clause in &cnf.clauses {
        match inspect_clause(clause, &state.values)? {
            ClauseState::Unit(literal) => {
                if state.assign(literal.variable, literal.polarity)? == Assignment::Conflict {
                    return Ok(false);
                }
            }
            ClauseState::Conflict => return Ok(false),
            ClauseState::Satisfied | ClauseState::Unresolved => {}
        }
    }
    Ok(true)
}

fn propagate(
    cnf: &Cnf,
    clause_index: &ClauseIndex,
    state: &mut SearchState,
) -> Result<bool, DebtScanError> {
    while state.propagation_head < state.trail.len() {
        let Some(&variable) = state.trail.get(state.propagation_head) else {
            return Err(DebtScanError::CfgSatisfiability);
        };
        state.propagation_head += 1;
        let Some(value) = state.values.get(variable).copied().flatten() else {
            return Err(DebtScanError::CfgSatisfiability);
        };
        for &clause_number in clause_index.clauses_falsified_by(variable, value)? {
            let Some(clause) = cnf.clauses.get(clause_number) else {
                return Err(DebtScanError::CfgSatisfiability);
            };
            match inspect_clause(clause, &state.values)? {
                ClauseState::Unit(literal) => {
                    if state.assign(literal.variable, literal.polarity)? == Assignment::Conflict {
                        return Ok(false);
                    }
                }
                ClauseState::Conflict => return Ok(false),
                ClauseState::Satisfied | ClauseState::Unresolved => {}
            }
        }
    }
    Ok(true)
}

fn inspect_clause(
    clause: &[Literal],
    values: &[Option<bool>],
) -> Result<ClauseState, DebtScanError> {
    let mut unassigned = None;
    let mut multiple_unassigned = false;
    for literal in clause {
        let Some(value) = values.get(literal.variable) else {
            return Err(DebtScanError::CfgSatisfiability);
        };
        match value {
            Some(value) if *value == literal.polarity => return Ok(ClauseState::Satisfied),
            Some(_) => {}
            None if unassigned.is_none() => unassigned = Some(*literal),
            None => multiple_unassigned = true,
        }
    }
    match (unassigned, multiple_unassigned) {
        (None, _) => Ok(ClauseState::Conflict),
        (Some(literal), false) => Ok(ClauseState::Unit(literal)),
        (Some(_), true) => Ok(ClauseState::Unresolved),
    }
}

#[cfg(test)]
#[path = "solver_tests.rs"]
mod tests;
