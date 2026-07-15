use crate::{
    CONTRACT_HASH_HEX_BYTES, ContractDescriptorDigest, ContractError, MAX_WIRE_TEXT_BYTES,
    PORTABLE_CONTRACT_SCHEMA, validate_contract_text_field, validate_crm_tag,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedContractDescriptor {
    canonical_json: String,
    contract_schema: String,
    crm_namespace: String,
    crm_name: String,
    crm_version: String,
    abi_hash: String,
    signature_hash: String,
    descriptor_sha256: ContractDescriptorDigest,
}

impl ValidatedContractDescriptor {
    pub fn from_json(json_bytes: &[u8]) -> Result<Self, ContractError> {
        let value: Value = serde_json::from_slice(json_bytes)
            .map_err(|error| ContractError::InvalidJson(error.to_string()))?;
        validate_portable_contract_descriptor_value(&value)?;

        let canonical_json = canonical_json(&value);
        let root = object_at(&value, "$")?;
        let crm = object_at(required(root, "$", "crm")?, "$.crm")?;
        let fingerprints = object_at(required(root, "$", "fingerprints")?, "$.fingerprints")?;
        let descriptor_sha256 =
            ContractDescriptorDigest::parse(sha256_hex(canonical_json.as_bytes()))?;

        Ok(Self {
            canonical_json,
            contract_schema: string_at(required(root, "$", "schema")?, "$.schema")?.to_string(),
            crm_namespace: string_at(required(crm, "$.crm", "namespace")?, "$.crm.namespace")?
                .to_string(),
            crm_name: string_at(required(crm, "$.crm", "name")?, "$.crm.name")?.to_string(),
            crm_version: string_at(required(crm, "$.crm", "version")?, "$.crm.version")?
                .to_string(),
            abi_hash: string_at(
                required(fingerprints, "$.fingerprints", "abi_hash")?,
                "$.fingerprints.abi_hash",
            )?
            .to_string(),
            signature_hash: string_at(
                required(fingerprints, "$.fingerprints", "signature_hash")?,
                "$.fingerprints.signature_hash",
            )?
            .to_string(),
            descriptor_sha256,
        })
    }

    pub fn canonical_json(&self) -> &str {
        &self.canonical_json
    }

    pub fn contract_schema(&self) -> &str {
        &self.contract_schema
    }

    pub fn crm_namespace(&self) -> &str {
        &self.crm_namespace
    }

    pub fn crm_name(&self) -> &str {
        &self.crm_name
    }

    pub fn crm_version(&self) -> &str {
        &self.crm_version
    }

    pub fn abi_hash(&self) -> &str {
        &self.abi_hash
    }

    pub fn signature_hash(&self) -> &str {
        &self.signature_hash
    }

    pub fn descriptor_sha256(&self) -> &ContractDescriptorDigest {
        &self.descriptor_sha256
    }
}

pub fn contract_descriptor_sha256_hex(json_bytes: &[u8]) -> Result<String, ContractError> {
    let value: Value = serde_json::from_slice(json_bytes)
        .map_err(|err| ContractError::InvalidJson(err.to_string()))?;
    Ok(sha256_hex(canonical_json(&value).as_bytes()))
}

pub fn validate_portable_contract_descriptor_json(json_bytes: &[u8]) -> Result<(), ContractError> {
    ValidatedContractDescriptor::from_json(json_bytes).map(|_| ())
}

pub fn validate_portable_contract_descriptor_value(value: &Value) -> Result<(), ContractError> {
    let root = object_at(value, "$")?;
    ensure_keys(root, "$", &["schema", "crm", "fingerprints", "methods"])?;
    let schema = string_at(required(root, "$", "schema")?, "$.schema")?;
    if schema != PORTABLE_CONTRACT_SCHEMA {
        return Err(invalid(
            "$.schema",
            format!("expected {PORTABLE_CONTRACT_SCHEMA:?}, got {schema:?}"),
        ));
    }

    let crm_path = "$.crm";
    let crm = object_at(required(root, "$", "crm")?, crm_path)?;
    ensure_keys(crm, crm_path, &["namespace", "name", "version"])?;
    let crm_ns = string_at(required(crm, crm_path, "namespace")?, "$.crm.namespace")?;
    let crm_name = string_at(required(crm, crm_path, "name")?, "$.crm.name")?;
    let crm_ver = string_at(required(crm, crm_path, "version")?, "$.crm.version")?;
    validate_crm_tag(crm_ns, crm_name, crm_ver)?;

    validate_fingerprints(required(root, "$", "fingerprints")?, "$.fingerprints")?;

    let methods = array_at(required(root, "$", "methods")?, "$.methods")?;
    let mut method_names = BTreeSet::new();
    for (index, method) in methods.iter().enumerate() {
        let method_path = format!("$.methods[{index}]");
        validate_method_descriptor(method, &method_path, &mut method_names)?;
    }
    Ok(())
}

fn validate_fingerprints(value: &Value, path: &str) -> Result<(), ContractError> {
    let object = object_at(value, path)?;
    ensure_keys(object, path, &["abi_hash", "signature_hash"])?;
    let abi_hash_path = format!("{path}.abi_hash");
    let abi_hash = string_at(required(object, path, "abi_hash")?, &abi_hash_path)?;
    validate_hash_text(&abi_hash_path, abi_hash)?;
    let signature_hash_path = format!("{path}.signature_hash");
    let signature_hash = string_at(
        required(object, path, "signature_hash")?,
        &signature_hash_path,
    )?;
    validate_hash_text(&signature_hash_path, signature_hash)
}

fn validate_method_descriptor(
    value: &Value,
    path: &str,
    method_names: &mut BTreeSet<String>,
) -> Result<(), ContractError> {
    let object = object_at(value, path)?;
    ensure_keys(
        object,
        path,
        &["access", "buffer", "name", "parameters", "return", "wire"],
    )?;
    let name_path = format!("{path}.name");
    let name = string_at(required(object, path, "name")?, &name_path)?;
    validate_contract_text_field("method name", name)?;
    if !method_names.insert(name.to_string()) {
        return Err(invalid(path, format!("duplicate method name {name:?}")));
    }

    let access_path = format!("{path}.access");
    let access = string_at(required(object, path, "access")?, &access_path)?;
    if !matches!(access, "read" | "write") {
        return Err(invalid(access_path, "access must be \"read\" or \"write\""));
    }

    let buffer_path = format!("{path}.buffer");
    validate_buffer(required(object, path, "buffer")?, &buffer_path)?;

    let params_path = format!("{path}.parameters");
    let parameters = array_at(required(object, path, "parameters")?, &params_path)?;
    let mut param_names = BTreeSet::new();
    for (index, param) in parameters.iter().enumerate() {
        let param_path = format!("{params_path}[{index}]");
        validate_parameter_descriptor(param, &param_path, &mut param_names)?;
    }

    let return_path = format!("{path}.return");
    validate_annotation(required(object, path, "return")?, &return_path)?;

    let wire_path = format!("{path}.wire");
    let wire = object_at(required(object, path, "wire")?, &wire_path)?;
    let input_path = format!("{wire_path}.input");
    validate_wire_ref(required(wire, &wire_path, "input")?, &input_path)?;
    let output_path = format!("{wire_path}.output");
    validate_wire_ref(required(wire, &wire_path, "output")?, &output_path)?;
    Ok(())
}

fn validate_parameter_descriptor(
    value: &Value,
    path: &str,
    param_names: &mut BTreeSet<String>,
) -> Result<(), ContractError> {
    let object = object_at(value, path)?;
    ensure_keys(object, path, &["default", "kind", "name", "type"])?;
    let name_path = format!("{path}.name");
    let name = string_at(required(object, path, "name")?, &name_path)?;
    validate_contract_text_field("parameter name", name)?;
    if !param_names.insert(name.to_string()) {
        return Err(invalid(path, format!("duplicate parameter name {name:?}")));
    }

    let kind_path = format!("{path}.kind");
    let kind = string_at(required(object, path, "kind")?, &kind_path)?;
    if !matches!(
        kind,
        "POSITIONAL_ONLY" | "POSITIONAL_OR_KEYWORD" | "KEYWORD_ONLY"
    ) {
        return Err(invalid(
            kind_path,
            "parameter kind must be POSITIONAL_ONLY, POSITIONAL_OR_KEYWORD, or KEYWORD_ONLY",
        ));
    }

    let default_path = format!("{path}.default");
    validate_default(required(object, path, "default")?, &default_path)?;
    let type_path = format!("{path}.type");
    validate_annotation(required(object, path, "type")?, &type_path)
}

fn validate_default(value: &Value, path: &str) -> Result<(), ContractError> {
    let object = object_at(value, path)?;
    let kind_path = format!("{path}.kind");
    let kind = string_at(required(object, path, "kind")?, &kind_path)?;
    match kind {
        "missing" => {
            ensure_keys(object, path, &["kind"])?;
            Ok(())
        }
        "json_scalar" => {
            ensure_keys(object, path, &["kind", "value"])?;
            let value_path = format!("{path}.value");
            let default_value = required(object, path, "value")?;
            if matches!(
                default_value,
                Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
            ) {
                Ok(())
            } else {
                Err(invalid(
                    value_path,
                    "json_scalar default must be null, bool, number, or string",
                ))
            }
        }
        _ => Err(invalid(
            kind_path,
            "default kind must be missing or json_scalar",
        )),
    }
}

fn validate_annotation(value: &Value, path: &str) -> Result<(), ContractError> {
    let object = object_at(value, path)?;
    let kind_path = format!("{path}.kind");
    let kind = string_at(required(object, path, "kind")?, &kind_path)?;
    match kind {
        "none" => {
            ensure_keys(object, path, &["kind"])?;
            Ok(())
        }
        "primitive" => {
            ensure_keys(object, path, &["kind", "name"])?;
            let name_path = format!("{path}.name");
            let name = string_at(required(object, path, "name")?, &name_path)?;
            if matches!(
                name,
                "bool" | "int" | "float" | "str" | "bytes" | "memoryview" | "bytearray"
            ) {
                Ok(())
            } else {
                Err(invalid(
                    name_path,
                    format!("unsupported primitive {name:?}"),
                ))
            }
        }
        "list" => {
            ensure_keys(object, path, &["item", "kind"])?;
            let item_path = format!("{path}.item");
            validate_annotation(required(object, path, "item")?, &item_path)
        }
        "dict" => {
            ensure_keys(object, path, &["key", "kind", "value"])?;
            let key_path = format!("{path}.key");
            validate_annotation(required(object, path, "key")?, &key_path)?;
            let value_path = format!("{path}.value");
            validate_annotation(required(object, path, "value")?, &value_path)
        }
        "tuple_variadic" => {
            ensure_keys(object, path, &["item", "kind"])?;
            let item_path = format!("{path}.item");
            validate_annotation(required(object, path, "item")?, &item_path)
        }
        "tuple" => {
            ensure_keys(object, path, &["items", "kind"])?;
            let items_path = format!("{path}.items");
            let items = array_at(required(object, path, "items")?, &items_path)?;
            if items.is_empty() {
                return Err(invalid(items_path, "tuple items cannot be empty"));
            }
            for (index, item) in items.iter().enumerate() {
                validate_annotation(item, &format!("{items_path}[{index}]"))?;
            }
            Ok(())
        }
        "union" => {
            ensure_keys(object, path, &["items", "kind"])?;
            let items_path = format!("{path}.items");
            let items = array_at(required(object, path, "items")?, &items_path)?;
            if items.is_empty() {
                return Err(invalid(items_path, "union items cannot be empty"));
            }
            for (index, item) in items.iter().enumerate() {
                validate_annotation(item, &format!("{items_path}[{index}]"))?;
            }
            Ok(())
        }
        "codec" => {
            ensure_keys(object, path, &["codec", "kind"])?;
            let codec_path = format!("{path}.codec");
            validate_codec_ref(required(object, path, "codec")?, &codec_path)
        }
        "transferable" => {
            ensure_keys(object, path, &["abi_ref", "kind"])?;
            let abi_path = format!("{path}.abi_ref");
            validate_wire_ref(required(object, path, "abi_ref")?, &abi_path)
        }
        _ => Err(invalid(
            kind_path,
            format!("unsupported annotation kind {kind:?}"),
        )),
    }
}

fn validate_wire_ref(value: &Value, path: &str) -> Result<(), ContractError> {
    if value.is_null() {
        return Ok(());
    }
    if value
        .get("family")
        .and_then(Value::as_str)
        .is_some_and(|family| family == "python-pickle-default")
    {
        return Err(invalid(path, "python-pickle-default is not portable"));
    }
    let object = object_at(value, path)?;
    let kind_path = format!("{path}.kind");
    let kind = string_at(required(object, path, "kind")?, &kind_path)?;
    if kind != "codec_ref" {
        return Err(invalid(
            kind_path,
            "portable wire refs must use kind \"codec_ref\"",
        ));
    }
    validate_codec_ref(value, path)
}

fn validate_codec_ref(value: &Value, path: &str) -> Result<(), ContractError> {
    let object = object_at(value, path)?;
    let kind_path = format!("{path}.kind");
    let kind = string_at(required(object, path, "kind")?, &kind_path)?;
    if kind != "codec_ref" {
        return Err(invalid(kind_path, "codec ref kind must be \"codec_ref\""));
    }
    ensure_keys(
        object,
        path,
        &[
            "capabilities",
            "id",
            "kind",
            "media_type",
            "portable",
            "schema",
            "schema_sha256",
            "version",
        ],
    )?;

    validate_identity_value(required(object, path, "id")?, &format!("{path}.id"))?;
    validate_identity_value(
        required(object, path, "version")?,
        &format!("{path}.version"),
    )?;
    if let Some(schema) = object.get("schema") {
        validate_identity_value(schema, &format!("{path}.schema"))?;
    }
    if let Some(media_type) = object.get("media_type") {
        validate_identity_value(media_type, &format!("{path}.media_type"))?;
    }
    if let Some(schema_sha256) = object.get("schema_sha256") {
        let sha_path = format!("{path}.schema_sha256");
        let value = string_at(schema_sha256, &sha_path)?;
        validate_hash_text(&sha_path, value)?;
    }

    let portable_path = format!("{path}.portable");
    let portable = required(object, path, "portable")?
        .as_bool()
        .ok_or_else(|| invalid(&portable_path, "portable must be a boolean"))?;
    if !portable {
        return Err(invalid(
            portable_path,
            "portable codec refs must set portable=true",
        ));
    }

    if let Some(capabilities) = object.get("capabilities") {
        let capabilities_path = format!("{path}.capabilities");
        let items = array_at(capabilities, &capabilities_path)?;
        let mut seen = BTreeSet::new();
        for (index, capability) in items.iter().enumerate() {
            let capability_path = format!("{capabilities_path}[{index}]");
            let value = string_at(capability, &capability_path)?;
            validate_capability_text(&capability_path, value)?;
            if !seen.insert(value.to_string()) {
                return Err(invalid(
                    capability_path,
                    format!("duplicate capability {value:?}"),
                ));
            }
        }
    }
    Ok(())
}

fn validate_buffer(value: &Value, path: &str) -> Result<(), ContractError> {
    if value.is_null() {
        return Ok(());
    }
    let value = string_at(value, path)?;
    if matches!(value, "view" | "hold") {
        Ok(())
    } else {
        Err(invalid(path, "buffer must be null, \"view\", or \"hold\""))
    }
}

fn required<'a>(
    object: &'a serde_json::Map<String, Value>,
    path: &str,
    key: &'static str,
) -> Result<&'a Value, ContractError> {
    object
        .get(key)
        .ok_or_else(|| invalid(format!("{path}.{key}"), "required field is missing"))
}

fn ensure_keys(
    object: &serde_json::Map<String, Value>,
    path: &str,
    allowed: &[&'static str],
) -> Result<(), ContractError> {
    for key in object.keys() {
        if !allowed.iter().any(|allowed_key| *allowed_key == key) {
            return Err(invalid(
                format!("{path}.{key}"),
                format!("unknown field {key:?}"),
            ));
        }
    }
    Ok(())
}

fn object_at<'a>(
    value: &'a Value,
    path: &str,
) -> Result<&'a serde_json::Map<String, Value>, ContractError> {
    value
        .as_object()
        .ok_or_else(|| invalid(path, "expected object"))
}

fn array_at<'a>(value: &'a Value, path: &str) -> Result<&'a Vec<Value>, ContractError> {
    value
        .as_array()
        .ok_or_else(|| invalid(path, "expected array"))
}

fn string_at<'a>(value: &'a Value, path: &str) -> Result<&'a str, ContractError> {
    value
        .as_str()
        .ok_or_else(|| invalid(path, "expected string"))
}

fn validate_identity_value(value: &Value, path: &str) -> Result<(), ContractError> {
    let value = string_at(value, path)?;
    if value.is_empty() {
        return Err(invalid(path, "cannot be empty"));
    }
    if value.len() > MAX_WIRE_TEXT_BYTES {
        return Err(invalid(
            path,
            format!("cannot exceed {MAX_WIRE_TEXT_BYTES} bytes: {}", value.len()),
        ));
    }
    if value.trim() != value {
        return Err(invalid(
            path,
            "cannot contain leading or trailing whitespace",
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(invalid(path, "cannot contain control characters"));
    }
    let mut chars = value.chars();
    if !chars.next().is_some_and(|ch| ch.is_ascii_alphanumeric()) {
        return Err(invalid(path, "must start with an ASCII letter or digit"));
    }
    if !chars
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | ':' | '/' | '+' | '-'))
    {
        return Err(invalid(path, "contains unsupported characters"));
    }
    Ok(())
}

fn validate_capability_text(path: &str, value: &str) -> Result<(), ContractError> {
    if value.is_empty() {
        return Err(invalid(path, "capability cannot be empty"));
    }
    if value.trim() != value {
        return Err(invalid(
            path,
            "capability cannot contain leading or trailing whitespace",
        ));
    }
    let mut chars = value.chars();
    if !chars.next().is_some_and(|ch| ch.is_ascii_alphanumeric()) {
        return Err(invalid(
            path,
            "capability must start with an ASCII letter or digit",
        ));
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '+' | '-')) {
        return Err(invalid(path, "capability contains unsupported characters"));
    }
    Ok(())
}

fn validate_hash_text(path: &str, value: &str) -> Result<(), ContractError> {
    if value.len() != CONTRACT_HASH_HEX_BYTES {
        return Err(invalid(path, "must be exactly 64 lowercase hex bytes"));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(invalid(path, "must be exactly 64 lowercase hex bytes"));
    }
    Ok(())
}

fn invalid(path: impl Into<String>, message: impl Into<String>) -> ContractError {
    ContractError::InvalidDescriptor {
        path: path.into(),
        message: message.into(),
    }
}

pub(crate) fn canonical_json(value: &Value) -> String {
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

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    lower_hex(&digest)
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
