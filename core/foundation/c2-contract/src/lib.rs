//! Language-neutral route contract validation and descriptor hashing.

use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MAX_WIRE_TEXT_BYTES: usize = u8::MAX as usize;
pub const CONTRACT_HASH_HEX_BYTES: usize = 64;

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
    if value.as_bytes().len() != CONTRACT_HASH_HEX_BYTES {
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

pub fn contract_descriptor_sha256_hex(json_bytes: &[u8]) -> Result<String, ContractError> {
    let value: Value = serde_json::from_slice(json_bytes)
        .map_err(|err| ContractError::InvalidJson(err.to_string()))?;
    let canonical = canonical_json(&value);
    let digest = Sha256::digest(canonical.as_bytes());
    Ok(lower_hex(&digest))
}

fn validate_wire_len(field: &'static str, value: &str) -> Result<(), ContractError> {
    let actual = value.as_bytes().len();
    if actual > MAX_WIRE_TEXT_BYTES {
        return Err(ContractError::TooLong {
            field,
            max: MAX_WIRE_TEXT_BYTES,
            actual,
        });
    }
    Ok(())
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => value.to_string(),
        Value::Array(values) => {
            let body = values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",");
            format!("[{body}]")
        }
        Value::Object(map) => {
            let body = map
                .iter()
                .map(|(key, value)| {
                    let encoded_key = serde_json::to_string(key).expect("JSON object key encodes");
                    format!("{encoded_key}:{}", canonical_json(value))
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{body}}}")
        }
    }
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
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
}
