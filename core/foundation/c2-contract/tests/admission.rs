use c2_contract::{
    ContractError, ContractLimitMetric, ContractRelease, ContractReleaseRef,
    MAX_CONTRACT_JSON_VALUES, MAX_CONTRACT_METHODS, MAX_CONTRACT_NESTING_DEPTH,
    MAX_CONTRACT_SOURCE_BYTES, contract_descriptor_sha256_hex, derive_contract_fingerprints_json,
};
use std::sync::Mutex;

const DESCRIPTOR: &str =
    include_str!("../../../../tests/fixtures/contracts/portable-release.contract.json");
const REFERENCE: &str =
    include_str!("../../../../tests/fixtures/contracts/portable-release.ref.json");

static LARGE_TEST_LOCK: Mutex<()> = Mutex::new(());

fn assert_limit(
    error: ContractError,
    metric: ContractLimitMetric,
    limit: u64,
    observed: u64,
    path: &str,
) {
    assert_eq!(
        error,
        ContractError::LimitExceeded {
            metric,
            limit,
            observed,
            path: path.to_string(),
        }
    );
}

#[test]
fn source_bytes_accept_the_boundary_and_reject_one_over_before_parsing() {
    let _large_test = LARGE_TEST_LOCK.lock().unwrap();
    let limit = usize::try_from(MAX_CONTRACT_SOURCE_BYTES).unwrap();
    let mut exact = Vec::with_capacity(limit);
    exact.push(b'"');
    exact.resize(limit - 1, b'x');
    exact.push(b'"');
    assert_eq!(exact.len(), limit);
    contract_descriptor_sha256_hex(&exact).unwrap();

    let one_over = vec![0; limit + 1];
    let error = contract_descriptor_sha256_hex(&one_over).unwrap_err();
    assert_limit(
        error,
        ContractLimitMetric::SourceBytes,
        MAX_CONTRACT_SOURCE_BYTES,
        u64::try_from(limit + 1).unwrap(),
        "$",
    );
}

#[test]
fn json_values_accept_the_boundary_and_reject_one_over() {
    let _large_test = LARGE_TEST_LOCK.lock().unwrap();
    let limit = usize::try_from(MAX_CONTRACT_JSON_VALUES).unwrap();
    let mut exact = Vec::with_capacity(limit * 2);
    exact.push(b'[');
    for index in 0..(limit - 1) {
        if index != 0 {
            exact.push(b',');
        }
        exact.push(b'0');
    }
    exact.push(b']');
    contract_descriptor_sha256_hex(&exact).unwrap();

    exact.pop();
    exact.extend_from_slice(b",0]");
    let error = contract_descriptor_sha256_hex(&exact).unwrap_err();
    assert_limit(
        error,
        ContractLimitMetric::JsonValues,
        MAX_CONTRACT_JSON_VALUES,
        u64::try_from(limit + 1).unwrap(),
        &format!("$[{}]", limit - 1),
    );
}

#[test]
fn release_reference_accepts_the_fixed_policy() {
    ContractReleaseRef::from_json(REFERENCE.as_bytes()).unwrap();
}

#[test]
fn depth_accepts_the_boundary_and_rejects_one_over_inside_the_visitor() {
    let exact_depth = usize::try_from(MAX_CONTRACT_NESTING_DEPTH).unwrap();
    let exact = nested_array_json(exact_depth);
    contract_descriptor_sha256_hex(exact.as_bytes()).unwrap();

    let one_over = nested_array_json(exact_depth + 1);
    let error = contract_descriptor_sha256_hex(one_over.as_bytes()).unwrap_err();
    assert_limit(
        error,
        ContractLimitMetric::JsonDepth,
        MAX_CONTRACT_NESTING_DEPTH,
        u64::try_from(exact_depth + 1).unwrap(),
        &format!("${}", "[0]".repeat(exact_depth)),
    );
}

#[test]
fn deeply_nested_hostile_descriptor_is_rejected_during_bounded_admission() {
    let mut descriptor: serde_json::Value = serde_json::from_str(DESCRIPTOR).unwrap();
    let nested_spec: serde_json::Value = serde_json::from_str(&nested_array_json(124)).unwrap();
    descriptor["methods"][1]["bindings"]["input"]["spec"] = nested_spec;
    let source = descriptor.to_string();

    let error = ContractRelease::from_descriptor_json(source.as_bytes()).unwrap_err();
    assert_limit(
        error,
        ContractLimitMetric::JsonDepth,
        MAX_CONTRACT_NESTING_DEPTH,
        MAX_CONTRACT_NESTING_DEPTH + 1,
        &format!("$.methods[1].bindings.input.spec{}", "[0]".repeat(123)),
    );
}

#[test]
fn method_limit_accepts_the_hard_boundary_and_rejects_one_over_before_iteration() {
    let exact = descriptor_with_methods(MAX_CONTRACT_METHODS, true);
    ContractRelease::from_descriptor_json(exact.as_bytes()).unwrap();

    let one_over = descriptor_with_methods(MAX_CONTRACT_METHODS + 1, false);
    let error = ContractRelease::from_descriptor_json(one_over.as_bytes()).unwrap_err();
    assert_limit(
        error,
        ContractLimitMetric::Methods,
        u64::try_from(MAX_CONTRACT_METHODS).unwrap(),
        u64::try_from(MAX_CONTRACT_METHODS + 1).unwrap(),
        "$.methods",
    );
}

fn nested_array_json(depth: usize) -> String {
    assert!(depth >= 1);
    format!("{}0{}", "[".repeat(depth - 1), "]".repeat(depth - 1))
}

fn descriptor_with_methods(count: usize, derive_fingerprints: bool) -> String {
    let mut descriptor: serde_json::Value = serde_json::from_str(DESCRIPTOR).unwrap();
    let template = descriptor["methods"][0].clone();
    descriptor["methods"] = serde_json::Value::Array(
        (0..count)
            .map(|index| {
                let mut method = template.clone();
                method["name"] = serde_json::Value::String(format!("method_{index:03}"));
                method
            })
            .collect(),
    );
    if derive_fingerprints {
        let fingerprints =
            derive_contract_fingerprints_json(descriptor.to_string().as_bytes()).unwrap();
        descriptor["fingerprints"]["abi_hash"] =
            serde_json::Value::String(fingerprints.abi_hash().to_string());
        descriptor["fingerprints"]["signature_hash"] =
            serde_json::Value::String(fingerprints.signature_hash().to_string());
    }
    descriptor.to_string()
}
