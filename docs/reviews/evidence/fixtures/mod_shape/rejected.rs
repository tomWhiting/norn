//! Every production item here violates the `mod.rs` shape contract.

use private_alias::PrivateType;

const PRODUCTION_CONST: usize = 1;
static PRODUCTION_STATIC: usize = PRODUCTION_CONST;

fn production_logic() {
    let production_local = PRODUCTION_STATIC;
}

struct ProductionType {
    production_field: usize,
}

enum ProductionEnum {
    ProductionVariant,
}

union ProductionUnion {
    production_union_field: usize,
}

trait ProductionTrait {
    type Associated;
    const REQUIRED_CONST: usize;
    fn required_method();
}

impl ProductionType {
    fn inherent_method(&self) -> usize {
        self.production_field
    }
}

type ProductionAlias = ProductionType;

extern crate core as production_core;

extern "C" {
    fn foreign_symbol();
    static FOREIGN_STATIC: usize;
}

macro_rules! production_macro {
    () => {};
}

production_macro!();

mod inline_logic {
    pub fn nested_logic() {}
}
