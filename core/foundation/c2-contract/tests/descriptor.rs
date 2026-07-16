use c2_contract::{ContractRelease, ValidatedContractDescriptor};

const SOURCE: &str =
    include_str!("../../../../tests/fixtures/contracts/portable-release.contract.json");
const CANONICAL: &str =
    include_str!("../../../../tests/fixtures/contracts/portable-release.canonical.json");
const DIGEST: &str = "cfe58b74b47efd2a866120cce103049c68284dde2cf66cfb95cce920ad9c9867";

#[test]
fn validated_descriptor_extracts_identity_and_canonical_content() {
    let descriptor = ValidatedContractDescriptor::from_json(SOURCE.as_bytes()).unwrap();

    assert_eq!(descriptor.canonical_json(), CANONICAL.trim_end());
    assert_eq!(descriptor.contract_schema(), "c-two.contract.v1");
    assert_eq!(descriptor.crm_namespace(), "test.contract-release");
    assert_eq!(descriptor.crm_name(), "Portable");
    assert_eq!(descriptor.crm_version(), "0.1.0");
    assert_eq!(
        descriptor.abi_hash(),
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    );
    assert_eq!(
        descriptor.signature_hash(),
        "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
    );
    assert_eq!(descriptor.descriptor_sha256().as_str(), DIGEST);
}

#[test]
fn formatting_and_key_order_do_not_change_descriptor_identity() {
    let source = ValidatedContractDescriptor::from_json(SOURCE.as_bytes()).unwrap();
    let source_release = ContractRelease::from_descriptor_json(SOURCE.as_bytes()).unwrap();
    let source_value: serde_json::Value = serde_json::from_str(SOURCE).unwrap();
    let reordered_json = format!(
        "{{\"methods\":{},\"fingerprints\":{},\"crm\":{},\"schema\":\"c-two.contract.v1\"}}",
        source_value["methods"], source_value["fingerprints"], source_value["crm"],
    );
    let reordered = ValidatedContractDescriptor::from_json(reordered_json.as_bytes()).unwrap();
    let reordered_release =
        ContractRelease::from_descriptor_json(reordered_json.as_bytes()).unwrap();

    assert_eq!(source.canonical_json(), reordered.canonical_json());
    assert_eq!(source.descriptor_sha256(), reordered.descriptor_sha256());
    assert_eq!(source_release.reference(), reordered_release.reference());
}

#[test]
fn descriptor_content_change_changes_digest() {
    let original = ValidatedContractDescriptor::from_json(SOURCE.as_bytes()).unwrap();
    let changed = SOURCE.replace("\"name\": \"ping\"", "\"name\": \"health\"");
    let changed = ValidatedContractDescriptor::from_json(changed.as_bytes()).unwrap();

    assert_ne!(original.descriptor_sha256(), changed.descriptor_sha256());
}
