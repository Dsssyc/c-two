//! Language-neutral route contract validation and descriptor hashing.

use thiserror::Error;

mod descriptor;
mod release;

pub use descriptor::{
    ValidatedContractDescriptor, contract_descriptor_sha256_hex,
    validate_portable_contract_descriptor_json, validate_portable_contract_descriptor_value,
};
pub use release::ContractDescriptorDigest;

#[cfg(test)]
use serde_json::Value;

pub const MAX_WIRE_TEXT_BYTES: usize = u8::MAX as usize;
pub const CONTRACT_HASH_HEX_BYTES: usize = 64;
pub const PORTABLE_CONTRACT_SCHEMA: &str = "c-two.contract.v1";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExpectedRouteContract {
    pub route_name: String,
    pub crm_ns: String,
    pub crm_name: String,
    pub crm_ver: String,
    pub abi_hash: String,
    pub signature_hash: String,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ContractError {
    #[error("{field} cannot be empty")]
    Empty { field: &'static str },
    #[error("{field} cannot exceed {max} bytes: {actual}")]
    TooLong {
        field: &'static str,
        max: usize,
        actual: usize,
    },
    #[error("{field} cannot contain leading or trailing whitespace")]
    SurroundingWhitespace { field: &'static str },
    #[error("{field} cannot contain control characters")]
    ControlCharacter { field: &'static str },
    #[error("{field} cannot contain path or tag separators")]
    Separator { field: &'static str },
    #[error("{field} must be exactly 64 lowercase hex bytes")]
    InvalidHash { field: &'static str },
    #[error("contract descriptor must be valid JSON: {0}")]
    InvalidJson(String),
    #[error("contract descriptor invalid at {path}: {message}")]
    InvalidDescriptor { path: String, message: String },
}

pub fn validate_named_route_name(field: &'static str, value: &str) -> Result<(), ContractError> {
    validate_route_text_field(field, value)
}

pub fn validate_call_route_key(field: &'static str, value: &str) -> Result<(), ContractError> {
    validate_route_text_field(field, value)
}

pub fn validate_contract_text_field(field: &'static str, value: &str) -> Result<(), ContractError> {
    if value.is_empty() {
        return Err(ContractError::Empty { field });
    }
    validate_wire_len(field, value)?;
    if value.trim() != value {
        return Err(ContractError::SurroundingWhitespace { field });
    }
    if value.chars().any(char::is_control) {
        return Err(ContractError::ControlCharacter { field });
    }
    if value.contains('/') || value.contains('\\') {
        return Err(ContractError::Separator { field });
    }
    Ok(())
}

fn validate_route_text_field(field: &'static str, value: &str) -> Result<(), ContractError> {
    if value.is_empty() {
        return Err(ContractError::Empty { field });
    }
    validate_wire_len(field, value)?;
    if value.trim() != value {
        return Err(ContractError::SurroundingWhitespace { field });
    }
    if value.chars().any(char::is_control) {
        return Err(ContractError::ControlCharacter { field });
    }
    if value.contains('\\') {
        return Err(ContractError::Separator { field });
    }
    Ok(())
}

pub fn validate_crm_tag(crm_ns: &str, crm_name: &str, crm_ver: &str) -> Result<(), ContractError> {
    validate_contract_text_field("crm namespace", crm_ns)?;
    validate_contract_text_field("crm name", crm_name)?;
    validate_contract_text_field("crm version", crm_ver)?;
    Ok(())
}

pub fn validate_contract_hash(field: &'static str, value: &str) -> Result<(), ContractError> {
    if value.len() != CONTRACT_HASH_HEX_BYTES {
        return Err(ContractError::InvalidHash { field });
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(ContractError::InvalidHash { field });
    }
    Ok(())
}

pub fn validate_expected_route_contract(
    expected: &ExpectedRouteContract,
) -> Result<(), ContractError> {
    validate_named_route_name("route name", &expected.route_name)?;
    validate_crm_tag(&expected.crm_ns, &expected.crm_name, &expected.crm_ver)?;
    validate_contract_hash("abi_hash", &expected.abi_hash)?;
    validate_contract_hash("signature_hash", &expected.signature_hash)?;
    Ok(())
}

fn validate_wire_len(field: &'static str, value: &str) -> Result<(), ContractError> {
    let actual = value.len();
    if actual > MAX_WIRE_TEXT_BYTES {
        return Err(ContractError::TooLong {
            field,
            max: MAX_WIRE_TEXT_BYTES,
            actual,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_hash_shape() {
        validate_contract_hash(
            "abi_hash",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        assert!(validate_contract_hash("abi_hash", "").is_err());
        assert!(
            validate_contract_hash(
                "abi_hash",
                "ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789",
            )
            .is_err()
        );
    }

    #[test]
    fn call_route_key_rejects_empty_like_named_route() {
        assert!(validate_call_route_key("route name", "").is_err());
        assert!(validate_named_route_name("route name", "").is_err());
    }

    #[test]
    fn route_name_allows_forward_slash_but_rejects_backslash() {
        validate_named_route_name("route name", "toodle/grid/0").unwrap();
        validate_call_route_key("route name", "toodle/grid/0").unwrap();
        assert!(validate_named_route_name("route name", "bad\\route").is_err());
    }

    #[test]
    fn descriptor_hash_canonicalizes_object_order() {
        let left =
            contract_descriptor_sha256_hex(br#"{"b":2,"a":{"y":1,"x":[true,null]}}"#).unwrap();
        let right =
            contract_descriptor_sha256_hex(br#"{"a":{"x":[true,null],"y":1},"b":2}"#).unwrap();
        assert_eq!(left, right);
        assert_eq!(left.len(), 64);
    }

    #[test]
    fn portable_descriptor_accepts_codec_refs_and_null_wire() {
        let descriptor = valid_portable_descriptor();
        validate_portable_contract_descriptor_json(descriptor.to_string().as_bytes()).unwrap();
    }

    #[test]
    fn portable_descriptor_accepts_route_contract_fingerprints() {
        let mut descriptor = valid_portable_descriptor();
        descriptor["fingerprints"] = serde_json::json!({
            "abi_hash": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "signature_hash": "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
        });

        validate_portable_contract_descriptor_json(descriptor.to_string().as_bytes()).unwrap();
    }

    #[test]
    fn portable_descriptor_rejects_invalid_route_contract_fingerprints() {
        let mut descriptor = valid_portable_descriptor();
        descriptor["fingerprints"] = serde_json::json!({
            "abi_hash": "bad",
            "signature_hash": "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
        });

        let err = validate_portable_contract_descriptor_json(descriptor.to_string().as_bytes())
            .unwrap_err();
        assert!(err.to_string().contains("abi_hash"));
    }

    #[test]
    fn portable_descriptor_requires_route_contract_fingerprints() {
        let mut descriptor = valid_portable_descriptor();
        descriptor.as_object_mut().unwrap().remove("fingerprints");

        let err = validate_portable_contract_descriptor_json(descriptor.to_string().as_bytes())
            .unwrap_err();
        assert!(err.to_string().contains("fingerprints"));
    }

    #[test]
    fn portable_descriptor_rejects_wrong_schema() {
        let mut descriptor = valid_portable_descriptor();
        descriptor["schema"] = Value::String("c-two.python.crm.descriptor.v2".to_string());

        let err = validate_portable_contract_descriptor_json(descriptor.to_string().as_bytes())
            .unwrap_err();
        assert!(err.to_string().contains("schema"));
    }

    #[test]
    fn portable_descriptor_rejects_pickle_wire_refs() {
        let mut descriptor = valid_portable_descriptor();
        descriptor["methods"][0]["wire"]["input"] = serde_json::json!({
            "family": "python-pickle-default",
            "kind": "builtin",
            "portable": false,
            "version": "pickle-protocol-5"
        });

        let err = validate_portable_contract_descriptor_json(descriptor.to_string().as_bytes())
            .unwrap_err();
        assert!(err.to_string().contains("python-pickle-default"));
    }

    #[test]
    fn portable_descriptor_rejects_custom_wire_refs() {
        let mut descriptor = valid_portable_descriptor();
        descriptor["methods"][0]["wire"]["output"] = serde_json::json!({
            "kind": "custom_id",
            "value": "legacy"
        });

        let err = validate_portable_contract_descriptor_json(descriptor.to_string().as_bytes())
            .unwrap_err();
        assert!(err.to_string().contains("codec_ref"));
    }

    #[test]
    fn portable_descriptor_rejects_nonportable_codec_refs() {
        let mut descriptor = valid_portable_descriptor();
        descriptor["methods"][0]["wire"]["output"]["portable"] = Value::Bool(false);

        let err = validate_portable_contract_descriptor_json(descriptor.to_string().as_bytes())
            .unwrap_err();
        assert!(err.to_string().contains("portable"));
    }

    #[test]
    fn portable_descriptor_rejects_non_string_codec_identity_fields() {
        for (field, value) in [
            ("version", serde_json::json!(1)),
            ("schema", serde_json::json!(["example.schema.v1"])),
            ("schema_sha256", serde_json::json!(null)),
            ("media_type", serde_json::json!(false)),
        ] {
            let mut descriptor = valid_portable_descriptor();
            descriptor["methods"][0]["wire"]["output"][field] = value;

            let err = validate_portable_contract_descriptor_json(descriptor.to_string().as_bytes())
                .unwrap_err();
            assert!(
                err.to_string().contains(&format!("wire.output.{field}")),
                "unexpected error for {field}: {err}"
            );
            assert!(
                err.to_string().contains("expected string"),
                "unexpected error for {field}: {err}"
            );
        }
    }

    #[test]
    fn portable_descriptor_rejects_malformed_codec_capabilities() {
        let mut descriptor = valid_portable_descriptor();
        descriptor["methods"][0]["wire"]["output"]["capabilities"] =
            Value::String("bytes".to_string());

        let err = validate_portable_contract_descriptor_json(descriptor.to_string().as_bytes())
            .unwrap_err();
        assert!(err.to_string().contains("wire.output.capabilities"));
        assert!(err.to_string().contains("expected array"));

        let mut descriptor = valid_portable_descriptor();
        descriptor["methods"][0]["wire"]["output"]["capabilities"] =
            serde_json::json!(["bytes", 1]);

        let err = validate_portable_contract_descriptor_json(descriptor.to_string().as_bytes())
            .unwrap_err();
        assert!(err.to_string().contains("wire.output.capabilities[1]"));
        assert!(err.to_string().contains("expected string"));
    }

    #[test]
    fn portable_descriptor_rejects_bad_annotation_shape() {
        let mut descriptor = valid_portable_descriptor();
        descriptor["methods"][0]["parameters"][0]["type"] = serde_json::json!({
            "kind": "list"
        });

        let err = validate_portable_contract_descriptor_json(descriptor.to_string().as_bytes())
            .unwrap_err();
        assert!(err.to_string().contains("item"));
    }

    #[test]
    fn portable_descriptor_rejects_unknown_fields() {
        let mut descriptor = valid_portable_descriptor();
        descriptor["methods"][0]["wire"]["output"]["schem_sha256"] =
            Value::String("typo".to_string());

        let err = validate_portable_contract_descriptor_json(descriptor.to_string().as_bytes())
            .unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    fn valid_portable_descriptor() -> Value {
        serde_json::json!({
            "schema": "c-two.contract.v1",
            "crm": {
                "namespace": "test.contract",
                "name": "Portable",
                "version": "0.1.0"
            },
            "fingerprints": {
                "abi_hash": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "signature_hash": "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
            },
            "methods": [
                {
                    "access": "write",
                    "buffer": "view",
                    "name": "echo",
                    "parameters": [
                        {
                            "name": "value",
                            "kind": "POSITIONAL_OR_KEYWORD",
                            "default": {"kind": "missing"},
                            "type": {
                                "kind": "codec",
                                "codec": codec_ref()
                            }
                        }
                    ],
                    "return": {
                        "kind": "codec",
                        "codec": codec_ref()
                    },
                    "wire": {
                        "input": codec_ref(),
                        "output": codec_ref()
                    }
                },
                {
                    "access": "read",
                    "buffer": "view",
                    "name": "ping",
                    "parameters": [],
                    "return": {"kind": "none"},
                    "wire": {
                        "input": null,
                        "output": null
                    }
                }
            ]
        })
    }

    fn codec_ref() -> Value {
        serde_json::json!({
            "kind": "codec_ref",
            "id": "org.example.codec",
            "version": "1",
            "schema": "example.schema.v1",
            "schema_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "capabilities": ["bytes"],
            "media_type": "application/octet-stream",
            "portable": true
        })
    }
}
