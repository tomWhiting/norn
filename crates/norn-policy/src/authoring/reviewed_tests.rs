use super::{
    DebtReviewRequirement, LocReviewRequirement, P1ReviewInventory, P1ReviewedInputError,
    P1ReviewedInputs, REVIEW_INVENTORY_SCHEMA_VERSION,
};
use crate::RepositoryPath;
use crate::baseline::{
    GovernanceTable, OriginLedger, P1_BASE_COMMIT, P1_BASE_TREE,
    P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY, ProductionLocClass,
};
use crate::digest::{Digest, digest_json};
use crate::facts::{SourceInventoryEntry, source_inventory_identity};
use crate::version::{ANALYZER_VERSION, DIGEST_VERSION};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn accepts_exact_binding_and_domain_separates_inventory_identity() -> TestResult {
    let inventory = inventory(false)?;
    let reviewed = reviewed_document(&inventory, 0, 0)?;
    P1ReviewedInputs::decode_p1(reviewed.as_bytes(), &inventory)?;

    let unframed = serde_json::to_value(&inventory)?;
    assert_ne!(inventory.canonical_identity()?, digest_json(&unframed)?);
    let mut changed = inventory.clone();
    changed.base_tree.push_str("-changed");
    assert_ne!(
        inventory.canonical_identity()?,
        changed.canonical_identity()?
    );
    Ok(())
}

#[test]
fn rejects_every_binding_mismatch_before_semantic_review() -> TestResult {
    let inventory = inventory(false)?;
    let valid = reviewed_document(&inventory, 0, 0)?;
    let identity = inventory.canonical_identity()?.to_string();
    let invalid_documents = [
        valid.replacen(&identity, &digest(0x91).to_string(), 1),
        valid.replacen(inventory.base_commit(), "different-commit", 1),
        valid.replacen(inventory.base_tree(), "different-tree", 1),
        valid.replacen(
            &inventory.origin_digest().to_string(),
            &digest(0x92).to_string(),
            1,
        ),
    ];

    for document in invalid_documents {
        let semantically_invalid =
            document.replacen("owner_roles = []", "owner_roles = [\"z\", \"a\"]", 1);
        assert!(matches!(
            P1ReviewedInputs::decode_p1(semantically_invalid.as_bytes(), &inventory),
            Err(P1ReviewedInputError::InventoryBinding)
        ));
    }
    Ok(())
}

#[test]
fn rejects_duplicate_reviewed_rows_for_each_governance_table() -> TestResult {
    let inventory = inventory(true)?;
    for (loc_rows, debt_rows, expected) in
        [(2, 1, GovernanceTable::Loc), (1, 2, GovernanceTable::Debt)]
    {
        let document = reviewed_document(&inventory, loc_rows, debt_rows)?;
        assert!(matches!(
            P1ReviewedInputs::decode_p1(document.as_bytes(), &inventory),
            Err(P1ReviewedInputError::DuplicateGovernance { table }) if table == expected
        ));
    }
    Ok(())
}

#[test]
fn reviewed_inputs_cannot_author_governance_for_another_origin() -> TestResult {
    let origin = empty_origin(vec![source_entry("crates/sample/src/original.rs", 0x41)?])?;
    let mut inventory = inventory(false)?;
    inventory.base_commit = origin.base_commit().as_str().to_owned();
    inventory.base_tree = origin.base_tree().as_str().to_owned();
    inventory.origin_digest = origin.normalized_digest()?;
    let document = reviewed_document(&inventory, 0, 0)?;
    let reviewed = P1ReviewedInputs::decode_p1(document.as_bytes(), &inventory)?;

    reviewed.author_anchor_for_origin(&origin)?;
    let drifted = empty_origin(vec![source_entry("crates/sample/src/drifted.rs", 0x42)?])?;
    assert!(matches!(
        reviewed.author_anchor_for_origin(&drifted),
        Err(P1ReviewedInputError::OriginBinding)
    ));
    Ok(())
}

fn inventory(with_governance: bool) -> TestResult<P1ReviewInventory> {
    let loc_exceptions = if with_governance {
        vec![LocReviewRequirement {
            origin_id: digest(0x11),
            path: RepositoryPath::parse("crates/sample/src/lib.rs")?,
            loc_class: ProductionLocClass::Other,
            production_loc: 501,
            baseline_limit: 500,
        }]
    } else {
        Vec::new()
    };
    let debt_exceptions = if with_governance {
        vec![DebtReviewRequirement {
            origin_id: digest(0x22),
            path: RepositoryPath::parse("crates/sample/src/debt.rs")?,
            fingerprint: digest(0x23),
            ordinal: 0,
        }]
    } else {
        Vec::new()
    };
    Ok(P1ReviewInventory {
        schema_version: REVIEW_INVENTORY_SCHEMA_VERSION,
        base_commit: "review-base-commit".to_owned(),
        base_tree: "review-base-tree".to_owned(),
        origin_digest: digest(0x33),
        base_source_inventory: Vec::new(),
        current_source_inventory: Vec::new(),
        base_compile_test_fixtures: Vec::new(),
        current_compile_test_fixtures: Vec::new(),
        loc_exceptions,
        debt_exceptions,
        writer_operations: Vec::new(),
    })
}

fn reviewed_document(
    inventory: &P1ReviewInventory,
    loc_rows: usize,
    debt_rows: usize,
) -> TestResult<String> {
    let roles = if loc_rows == 0 && debt_rows == 0 {
        "[]"
    } else {
        "[\"policy-team\"]"
    };
    let mut document = format!(
        "schema_version = 1\ninventory_identity = \"{}\"\nbase_commit = \"{}\"\nbase_tree = \"{}\"\norigin_digest = \"{}\"\nowner_roles = {roles}\n",
        inventory.canonical_identity()?,
        inventory.base_commit(),
        inventory.base_tree(),
        inventory.origin_digest(),
    );
    if loc_rows == 0 {
        document.push_str("loc_exceptions = []\n");
    }
    if debt_rows == 0 {
        document.push_str("debt_exceptions = []\n");
    }
    document.push_str(&format!("writer_resolutions = \"{}\"\n", digest(0x44)));
    document.push_str(
        "writer_vocabulary = { families = [], shared_primitives = [], cleanup_reviews = [], false_positive_reviews = [] }\n",
    );
    document.push_str("writer_classifications = []\n");
    append_governance_rows(&mut document, "loc_exceptions", loc_rows, digest(0x11));
    append_governance_rows(&mut document, "debt_exceptions", debt_rows, digest(0x22));
    Ok(document)
}

fn append_governance_rows(document: &mut String, table: &str, count: usize, origin: Digest) {
    for _ in 0..count {
        document.push_str(&format!(
            "\n[[{table}]]\norigin_id = \"{origin}\"\nowner = \"policy-team\"\ndue_phase = \"P4\"\nremediation_record = \"review-001\"\n"
        ));
    }
}

fn source_entry(path: &str, content: u8) -> TestResult<SourceInventoryEntry> {
    Ok(SourceInventoryEntry {
        path: RepositoryPath::parse(path)?,
        content: digest(content),
        production: false,
        test_only: true,
    })
}

fn empty_origin(source_inventory: Vec<SourceInventoryEntry>) -> TestResult<OriginLedger> {
    let source_inventory_digest = source_inventory_identity(&source_inventory);
    let document = serde_json::json!({
        "schema_version": 1,
        "algorithms": {
            "analyzer": ANALYZER_VERSION,
            "digest": DIGEST_VERSION,
        },
        "base": {
            "commit": P1_BASE_COMMIT,
            "tree": P1_BASE_TREE,
        },
        "digests": {
            "repository_policy": digest(0x31),
            "source_inventory": source_inventory_digest,
            "generated_include_registry": P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY,
        },
        "source_inventory": source_inventory,
        "compile_test_fixtures": [],
        "production_files": [],
        "item_groups": [],
        "prohibited_debt": [],
        "writer_operations": [],
    });
    Ok(OriginLedger::decode_p1(&serde_json::to_vec(&document)?)?)
}

const fn digest(byte: u8) -> Digest {
    Digest::from_bytes([byte; 32])
}
