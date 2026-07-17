fn production_logic() {
    #[cfg(test)]
    test_only_statement();
}

#[cfg(test)]
fn test_only_item() {}

fn trailing_production_logic() {
    #[cfg(test)]
    trailing_test_only_statement();
}
