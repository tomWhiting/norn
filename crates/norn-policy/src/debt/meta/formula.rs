//! Exact iterative satisfiability for the closed cfg formula grammar.

use std::collections::BTreeMap;

use super::{Meta, MetaForm, MetaId, MetaTree};
use crate::debt::model::DebtScanError;
use crate::digest::{Digest, digest_bytes};

#[path = "solver.rs"]
mod solver;

use solver::{Cnf, Literal, SolveOutcome, solve};

pub(super) fn is_impossible(
    tree: &MetaTree,
    meta_id: MetaId,
    offset: usize,
) -> Result<bool, DebtScanError> {
    let formula = Formula::from_meta(tree, meta_id, offset)?;
    Ok(!formula.is_satisfiable()?)
}

type FormulaId = usize;

struct Formula {
    nodes: Vec<FormulaNode>,
    root: FormulaId,
}

enum FormulaNode {
    Atom(Digest),
    All(Vec<FormulaId>),
    Any(Vec<FormulaId>),
    Not(FormulaId),
}

#[derive(Clone, Copy)]
enum FormulaShape<'a> {
    Atom,
    All(&'a [MetaId]),
    Any(&'a [MetaId]),
    Not(MetaId),
}

impl Formula {
    fn from_meta(tree: &MetaTree, root: MetaId, offset: usize) -> Result<Self, DebtScanError> {
        let mut nodes = Vec::new();
        let mut converted = vec![None; tree.nodes.len()];
        let mut pending = vec![(root, false)];
        while let Some((meta_id, expanded)) = pending.pop() {
            let meta = tree.node(meta_id)?;
            let shape = formula_shape(meta, offset)?;
            if !expanded {
                match shape {
                    FormulaShape::Atom => {}
                    FormulaShape::All(parts) | FormulaShape::Any(parts) => {
                        pending.push((meta_id, true));
                        pending.extend(parts.iter().rev().map(|part| (*part, false)));
                        continue;
                    }
                    FormulaShape::Not(part) => {
                        pending.push((meta_id, true));
                        pending.push((part, false));
                        continue;
                    }
                }
            }

            let node = match shape {
                FormulaShape::Atom => FormulaNode::Atom(digest_bytes(&tree.normalized(meta_id)?)),
                FormulaShape::All(parts) => {
                    FormulaNode::All(converted_parts(parts, &converted, offset)?)
                }
                FormulaShape::Any(parts) => {
                    FormulaNode::Any(converted_parts(parts, &converted, offset)?)
                }
                FormulaShape::Not(part) => {
                    FormulaNode::Not(converted_part(part, &converted, offset)?)
                }
            };
            let formula_id = nodes.len();
            nodes.push(node);
            let Some(slot) = converted.get_mut(meta_id) else {
                return Err(DebtScanError::Attribute { offset });
            };
            *slot = Some(formula_id);
        }
        let root = converted_part(root, &converted, offset)?;
        Ok(Self { nodes, root })
    }

    fn is_satisfiable(&self) -> Result<bool, DebtScanError> {
        let encoded = CnfEncoder::encode_formula(self)?;
        match solve(&encoded)? {
            SolveOutcome::Satisfiable(_) => Ok(true),
            SolveOutcome::Unsatisfiable => Ok(false),
        }
    }
}

fn formula_shape(meta: &Meta, offset: usize) -> Result<FormulaShape<'_>, DebtScanError> {
    match (meta.simple_name(), &meta.form) {
        (Some("all"), MetaForm::List(parts)) => Ok(FormulaShape::All(parts)),
        (Some("any"), MetaForm::List(parts)) => Ok(FormulaShape::Any(parts)),
        (Some("not"), MetaForm::List(parts)) if parts.len() == 1 => Ok(FormulaShape::Not(parts[0])),
        (Some("not"), MetaForm::List(_)) => Err(DebtScanError::Attribute { offset }),
        _ => Ok(FormulaShape::Atom),
    }
}

fn converted_parts(
    parts: &[MetaId],
    converted: &[Option<FormulaId>],
    offset: usize,
) -> Result<Vec<FormulaId>, DebtScanError> {
    parts
        .iter()
        .map(|part| converted_part(*part, converted, offset))
        .collect()
}

fn converted_part(
    meta_id: MetaId,
    converted: &[Option<FormulaId>],
    offset: usize,
) -> Result<FormulaId, DebtScanError> {
    converted
        .get(meta_id)
        .copied()
        .flatten()
        .ok_or(DebtScanError::Attribute { offset })
}

#[derive(Default)]
struct CnfEncoder {
    atom_variables: BTreeMap<Digest, usize>,
    next_variable: usize,
    clauses: Vec<Vec<Literal>>,
}

impl CnfEncoder {
    fn encode_formula(formula: &Formula) -> Result<Cnf, DebtScanError> {
        let mut encoder = Self::default();
        let mut encoded_nodes = Vec::with_capacity(formula.nodes.len());
        for node in &formula.nodes {
            let variable = match node {
                FormulaNode::Atom(atom) => encoder.encode_atom(*atom)?,
                FormulaNode::All(parts) => encoder.encode_all(parts, &encoded_nodes)?,
                FormulaNode::Any(parts) => encoder.encode_any(parts, &encoded_nodes)?,
                FormulaNode::Not(part) => encoder.encode_not(*part, &encoded_nodes)?,
            };
            encoded_nodes.push(variable);
        }
        let root = encoded_part(formula.root, &encoded_nodes)?;
        encoder.clauses.push(vec![Literal::positive(root)]);
        Ok(Cnf::new(encoder.next_variable, encoder.clauses))
    }

    fn encode_atom(&mut self, atom: Digest) -> Result<usize, DebtScanError> {
        if let Some(variable) = self.atom_variables.get(&atom) {
            return Ok(*variable);
        }
        let variable = self.allocate_variable()?;
        self.atom_variables.insert(atom, variable);
        Ok(variable)
    }

    fn encode_all(
        &mut self,
        parts: &[FormulaId],
        encoded: &[usize],
    ) -> Result<usize, DebtScanError> {
        let variable = self.allocate_variable()?;
        let mut reverse = vec![Literal::positive(variable)];
        for part in parts {
            let child = encoded_part(*part, encoded)?;
            self.clauses
                .push(vec![Literal::negative(variable), Literal::positive(child)]);
            reverse.push(Literal::negative(child));
        }
        self.clauses.push(reverse);
        Ok(variable)
    }

    fn encode_any(
        &mut self,
        parts: &[FormulaId],
        encoded: &[usize],
    ) -> Result<usize, DebtScanError> {
        let variable = self.allocate_variable()?;
        let mut reverse = vec![Literal::negative(variable)];
        for part in parts {
            let child = encoded_part(*part, encoded)?;
            self.clauses
                .push(vec![Literal::positive(variable), Literal::negative(child)]);
            reverse.push(Literal::positive(child));
        }
        self.clauses.push(reverse);
        Ok(variable)
    }

    fn encode_not(&mut self, part: FormulaId, encoded: &[usize]) -> Result<usize, DebtScanError> {
        let variable = self.allocate_variable()?;
        let child = encoded_part(part, encoded)?;
        self.clauses
            .push(vec![Literal::negative(variable), Literal::negative(child)]);
        self.clauses
            .push(vec![Literal::positive(variable), Literal::positive(child)]);
        Ok(variable)
    }

    fn allocate_variable(&mut self) -> Result<usize, DebtScanError> {
        let variable = self.next_variable;
        let Some(next_variable) = variable.checked_add(1) else {
            return Err(DebtScanError::CfgSatisfiability);
        };
        self.next_variable = next_variable;
        Ok(variable)
    }
}

fn encoded_part(part: FormulaId, encoded: &[usize]) -> Result<usize, DebtScanError> {
    encoded
        .get(part)
        .copied()
        .ok_or(DebtScanError::CfgSatisfiability)
}
