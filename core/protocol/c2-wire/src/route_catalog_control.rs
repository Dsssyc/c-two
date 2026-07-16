use std::collections::BTreeSet;

use c2_error::{C2ErrorEnvelope, ERROR_WIRE_VERSION, ErrorCode};
use serde::{Deserialize, Serialize};

use crate::msg_type::MsgType;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteContractWire {
    pub route_name: String,
    pub crm_ns: String,
    pub crm_name: String,
    pub crm_ver: String,
    pub abi_hash: String,
    pub signature_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteStateWire {
    Pending,
    Ready,
    Draining,
    Closed,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteStateReasonWire {
    RegisterPrepared,
    RegisterCommitted,
    ExplicitUnregister,
    Shutdown,
    OwnerLeaseExpired,
    OwnerWatchDisconnected,
    OwnerRouteMissing,
    OwnerIdentityMismatch,
    ContractMismatch,
    CatalogCompacted,
    ProtocolViolation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteMethodWire {
    pub name: String,
    pub index: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteRecordWire {
    pub route_name: String,
    pub route_uid: String,
    pub route_revision: u64,
    pub catalog_revision: u64,
    pub owner_server_id: String,
    pub owner_server_instance_id: String,
    pub owner_epoch: u64,
    pub contract: RouteContractWire,
    pub methods: Vec<RouteMethodWire>,
    pub max_payload_size: u64,
    pub state: RouteStateWire,
    pub state_reason: Option<RouteStateReasonWire>,
    pub lease_deadline_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RouteSelector {
    All,
    RouteName { route_name: String },
    Contract { expected: RouteContractWire },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteListRequest {
    pub selector: RouteSelector,
    pub min_revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteListResponse {
    pub catalog_revision: u64,
    pub min_watch_revision: u64,
    pub routes: Vec<RouteRecordWire>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteLookupRequest {
    pub expected: RouteContractWire,
    pub observed_route_uid: Option<String>,
    pub observed_route_revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RouteLookupResponse {
    Ready {
        current: RouteRecordWire,
    },
    NotFound {
        route_name: String,
    },
    Removed {
        route_name: String,
        route_uid: Option<String>,
    },
    Closed {
        route_name: String,
        route_uid: String,
        reason: RouteStateReasonWire,
    },
    Stale {
        current: RouteRecordWire,
    },
    ContractMismatch {
        current: RouteRecordWire,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteWatchRequest {
    pub from_revision: u64,
    pub selector: RouteSelector,
    pub allow_heartbeat: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RouteWatchEvent {
    Added {
        record: RouteRecordWire,
    },
    Updated {
        record: RouteRecordWire,
    },
    Removed {
        route_name: String,
        route_uid: String,
        catalog_revision: u64,
        reason: RouteStateReasonWire,
    },
    Closed {
        route_name: String,
        route_uid: String,
        catalog_revision: u64,
        reason: RouteStateReasonWire,
    },
    Heartbeat {
        catalog_revision: u64,
    },
    Compacted {
        compacted_revision: u64,
        current_revision: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteAck {
    pub nonce: u64,
    pub applied_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteNack {
    pub nonce: u64,
    pub rejected_revision: u64,
    pub error: C2ErrorEnvelope,
}

pub fn encode_route_list_request(request: &RouteListRequest) -> Result<Vec<u8>, String> {
    validate_route_list_request(request)?;
    encode_json_payload(MsgType::RouteList, request)
}

pub fn decode_route_list_request(payload: &[u8]) -> Result<RouteListRequest, String> {
    let body = split_tag(payload, MsgType::RouteList)?;
    let request: RouteListRequest = serde_json::from_slice(body).map_err(|err| err.to_string())?;
    validate_route_list_request(&request)?;
    Ok(request)
}

pub fn encode_route_list_response(response: &RouteListResponse) -> Result<Vec<u8>, String> {
    validate_route_list_response(response)?;
    encode_json_payload(MsgType::RouteListAck, response)
}

pub fn decode_route_list_response(payload: &[u8]) -> Result<RouteListResponse, String> {
    let body = split_tag(payload, MsgType::RouteListAck)?;
    let response: RouteListResponse =
        serde_json::from_slice(body).map_err(|err| err.to_string())?;
    validate_route_list_response(&response)?;
    Ok(response)
}

pub fn encode_route_lookup_request(request: &RouteLookupRequest) -> Result<Vec<u8>, String> {
    validate_route_lookup_request(request)?;
    encode_json_payload(MsgType::RouteLookup, request)
}

pub fn decode_route_lookup_request(payload: &[u8]) -> Result<RouteLookupRequest, String> {
    let body = split_tag(payload, MsgType::RouteLookup)?;
    let request: RouteLookupRequest =
        serde_json::from_slice(body).map_err(|err| err.to_string())?;
    validate_route_lookup_request(&request)?;
    Ok(request)
}

pub fn encode_route_lookup_response(response: &RouteLookupResponse) -> Result<Vec<u8>, String> {
    validate_route_lookup_response(response)?;
    encode_json_payload(MsgType::RouteLookupAck, response)
}

pub fn decode_route_lookup_response(payload: &[u8]) -> Result<RouteLookupResponse, String> {
    let body = split_tag(payload, MsgType::RouteLookupAck)?;
    let response: RouteLookupResponse =
        serde_json::from_slice(body).map_err(|err| err.to_string())?;
    validate_route_lookup_response(&response)?;
    Ok(response)
}

pub fn encode_route_watch_request(request: &RouteWatchRequest) -> Result<Vec<u8>, String> {
    validate_route_watch_request(request)?;
    encode_json_payload(MsgType::RouteWatch, request)
}

pub fn decode_route_watch_request(payload: &[u8]) -> Result<RouteWatchRequest, String> {
    let body = split_tag(payload, MsgType::RouteWatch)?;
    let request: RouteWatchRequest = serde_json::from_slice(body).map_err(|err| err.to_string())?;
    validate_route_watch_request(&request)?;
    Ok(request)
}

pub fn encode_route_watch_event(event: &RouteWatchEvent) -> Result<Vec<u8>, String> {
    validate_route_watch_event(event)?;
    encode_json_payload(MsgType::RouteWatchEvent, event)
}

pub fn decode_route_watch_event(payload: &[u8]) -> Result<RouteWatchEvent, String> {
    let body = split_tag(payload, MsgType::RouteWatchEvent)?;
    let event: RouteWatchEvent = serde_json::from_slice(body).map_err(|err| err.to_string())?;
    validate_route_watch_event(&event)?;
    Ok(event)
}

pub fn encode_route_ack(ack: &RouteAck) -> Result<Vec<u8>, String> {
    encode_json_payload(MsgType::RouteAck, ack)
}

pub fn decode_route_ack(payload: &[u8]) -> Result<RouteAck, String> {
    let body = split_tag(payload, MsgType::RouteAck)?;
    serde_json::from_slice(body).map_err(|err| err.to_string())
}

pub fn encode_route_nack(nack: &RouteNack) -> Result<Vec<u8>, String> {
    validate_route_nack(nack)?;
    encode_json_payload(MsgType::RouteNack, nack)
}

pub fn decode_route_nack(payload: &[u8]) -> Result<RouteNack, String> {
    let body = split_tag(payload, MsgType::RouteNack)?;
    let nack: RouteNack = serde_json::from_slice(body).map_err(|err| err.to_string())?;
    validate_route_nack(&nack)?;
    Ok(nack)
}

fn encode_json_payload<T: Serialize>(tag: MsgType, value: &T) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec(value).map_err(|err| err.to_string())?;
    let mut payload = Vec::with_capacity(1 + json.len());
    payload.push(tag.as_byte());
    payload.extend_from_slice(&json);
    Ok(payload)
}

fn split_tag(payload: &[u8], expected: MsgType) -> Result<&[u8], String> {
    let (tag, body) = payload
        .split_first()
        .ok_or_else(|| "route catalog control payload is empty".to_string())?;
    if *tag != expected.as_byte() {
        return Err("route catalog control payload tag mismatch".to_string());
    }
    Ok(body)
}

fn validate_route_list_request(request: &RouteListRequest) -> Result<(), String> {
    validate_selector(&request.selector)
}

fn validate_route_list_response(response: &RouteListResponse) -> Result<(), String> {
    if response.min_watch_revision > response.catalog_revision.saturating_add(1) {
        return Err("min_watch_revision must not be ahead of catalog_revision + 1".to_string());
    }
    let mut route_names = BTreeSet::new();
    let mut route_uids = BTreeSet::new();
    for route in &response.routes {
        validate_route_record(route)?;
        if route.catalog_revision > response.catalog_revision {
            return Err("route catalog_revision is ahead of list catalog_revision".to_string());
        }
        if !route_names.insert(route.route_name.as_str()) {
            return Err(format!(
                "duplicate route_name in route list: {}",
                route.route_name
            ));
        }
        if !route_uids.insert(route.route_uid.as_str()) {
            return Err(format!(
                "duplicate route_uid in route list: {}",
                route.route_uid
            ));
        }
    }
    Ok(())
}

fn validate_route_lookup_request(request: &RouteLookupRequest) -> Result<(), String> {
    validate_contract(&request.expected)?;
    match (
        request.observed_route_uid.as_ref(),
        request.observed_route_revision,
    ) {
        (Some(_), Some(_)) | (None, None) => {}
        _ => {
            return Err(
                "observed route token must include both route_uid and route_revision".to_string(),
            );
        }
    }
    if let Some(uid) = &request.observed_route_uid {
        validate_route_uid(uid)?;
    }
    Ok(())
}

fn validate_route_lookup_response(response: &RouteLookupResponse) -> Result<(), String> {
    match response {
        RouteLookupResponse::Ready { current }
        | RouteLookupResponse::Stale { current }
        | RouteLookupResponse::ContractMismatch { current } => validate_route_record(current),
        RouteLookupResponse::NotFound { route_name } => validate_route_name(route_name),
        RouteLookupResponse::Removed {
            route_name,
            route_uid,
        } => {
            validate_route_name(route_name)?;
            if let Some(uid) = route_uid {
                validate_route_uid(uid)?;
            }
            Ok(())
        }
        RouteLookupResponse::Closed {
            route_name,
            route_uid,
            ..
        } => {
            validate_route_name(route_name)?;
            validate_route_uid(route_uid)
        }
    }
}

fn validate_route_watch_request(request: &RouteWatchRequest) -> Result<(), String> {
    validate_selector(&request.selector)
}

fn validate_route_watch_event(event: &RouteWatchEvent) -> Result<(), String> {
    match event {
        RouteWatchEvent::Added { record } | RouteWatchEvent::Updated { record } => {
            validate_route_record(record)
        }
        RouteWatchEvent::Removed {
            route_name,
            route_uid,
            ..
        }
        | RouteWatchEvent::Closed {
            route_name,
            route_uid,
            ..
        } => {
            validate_route_name(route_name)?;
            validate_route_uid(route_uid)
        }
        RouteWatchEvent::Heartbeat { .. } => Ok(()),
        RouteWatchEvent::Compacted {
            compacted_revision,
            current_revision,
        } => {
            if compacted_revision > current_revision {
                return Err("compacted_revision must not exceed current_revision".to_string());
            }
            Ok(())
        }
    }
}

fn validate_route_nack(nack: &RouteNack) -> Result<(), String> {
    validate_error_envelope(&nack.error)
}

fn validate_selector(selector: &RouteSelector) -> Result<(), String> {
    match selector {
        RouteSelector::All => Ok(()),
        RouteSelector::RouteName { route_name } => validate_route_name(route_name),
        RouteSelector::Contract { expected } => validate_contract(expected),
    }
}

fn validate_route_record(record: &RouteRecordWire) -> Result<(), String> {
    validate_route_name(&record.route_name)?;
    validate_route_uid(&record.route_uid)?;
    c2_config::validate_server_id(&record.owner_server_id)?;
    c2_config::validate_server_id(&record.owner_server_instance_id)?;
    validate_contract(&record.contract)?;
    if record.contract.route_name != record.route_name {
        return Err("route record contract route_name must match route_name".to_string());
    }
    if record.max_payload_size == 0 {
        return Err("max_payload_size must be > 0".to_string());
    }
    if record.methods.len() > crate::handshake::MAX_METHODS {
        return Err(format!(
            "method count is too large: {} > {}",
            record.methods.len(),
            crate::handshake::MAX_METHODS
        ));
    }
    let mut names = BTreeSet::new();
    let mut indexes = BTreeSet::new();
    for method in &record.methods {
        if method.name.is_empty() {
            return Err("method name must not be empty".to_string());
        }
        if method.name.len() > u8::MAX as usize {
            return Err(format!(
                "method name is too long: {} bytes > 255",
                method.name.len()
            ));
        }
        if !names.insert(method.name.as_str()) {
            return Err(format!("duplicate method name: {}", method.name));
        }
        if !indexes.insert(method.index) {
            return Err(format!("duplicate method index: {}", method.index));
        }
    }
    Ok(())
}

fn validate_contract(contract: &RouteContractWire) -> Result<(), String> {
    let expected = c2_contract::ExpectedRouteContract {
        route_name: contract.route_name.clone(),
        crm_ns: contract.crm_ns.clone(),
        crm_name: contract.crm_name.clone(),
        crm_ver: contract.crm_ver.clone(),
        abi_hash: contract.abi_hash.clone(),
        signature_hash: contract.signature_hash.clone(),
    };
    c2_contract::validate_expected_route_contract(&expected).map_err(|err| err.to_string())
}

fn validate_route_name(route_name: &str) -> Result<(), String> {
    c2_contract::validate_named_route_name("route_name", route_name).map_err(|err| err.to_string())
}

fn validate_route_uid(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("route_uid must not be empty".to_string());
    }
    if value.len() > c2_contract::MAX_WIRE_TEXT_BYTES {
        return Err(format!(
            "route_uid is too long: {} bytes > {}",
            value.len(),
            c2_contract::MAX_WIRE_TEXT_BYTES
        ));
    }
    if value.bytes().any(|b| b <= 0x20 || b == b'/' || b == b'\\') {
        return Err("route_uid contains an invalid character".to_string());
    }
    Ok(())
}

fn validate_error_envelope(error: &C2ErrorEnvelope) -> Result<(), String> {
    if error.version != ERROR_WIRE_VERSION {
        return Err(format!(
            "unsupported C2 error envelope version {}",
            error.version
        ));
    }
    let code = ErrorCode::try_from(error.code)
        .map_err(|_| format!("unknown C2 error code {}", error.code))?;
    if error.name != code.name() {
        return Err(format!(
            "C2 error envelope name mismatch for code {}: expected {}, got {}",
            error.code,
            code.name(),
            error.name
        ));
    }
    Ok(())
}
