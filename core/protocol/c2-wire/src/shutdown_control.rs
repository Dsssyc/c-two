use serde::{Deserialize, Serialize};

use crate::msg_type::MsgType;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShutdownControlRouteOutcome {
    pub route_name: String,
    pub active_drained: bool,
    pub closed_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectShutdownAck {
    pub acknowledged: bool,
    pub shutdown_started: bool,
    pub server_stopped: bool,
    pub route_outcomes: Vec<ShutdownControlRouteOutcome>,
}

pub fn encode_shutdown_ack(outcome: &DirectShutdownAck) -> Result<Vec<u8>, String> {
    let mut payload = Vec::with_capacity(1 + 128);
    payload.push(MsgType::ShutdownAck.as_byte());
    let json = serde_json::to_vec(outcome).map_err(|err| err.to_string())?;
    payload.extend_from_slice(&json);
    Ok(payload)
}

pub fn decode_shutdown_ack(payload: &[u8]) -> Result<DirectShutdownAck, String> {
    let (tag, body) = payload
        .split_first()
        .ok_or_else(|| "shutdown ack payload is empty".to_string())?;
    if *tag != MsgType::ShutdownAck.as_byte() {
        return Err("shutdown ack payload tag mismatch".to_string());
    }
    serde_json::from_slice(body).map_err(|err| err.to_string())
}

pub fn encode_shutdown_initiate() -> Vec<u8> {
    vec![MsgType::ShutdownClient.as_byte()]
}

pub fn decode_shutdown_initiate(payload: &[u8]) -> Result<(), String> {
    let (tag, body) = payload
        .split_first()
        .ok_or_else(|| "shutdown request payload is empty".to_string())?;
    if *tag != MsgType::ShutdownClient.as_byte() {
        return Err("shutdown request payload tag mismatch".to_string());
    }
    if !body.is_empty() {
        return Err("shutdown initiate payload must not include a body".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg_type::{SHUTDOWN_ACK_BYTES, SHUTDOWN_CLIENT_BYTES};

    #[test]
    fn shutdown_ack_round_trip_preserves_outcome() {
        let outcome = DirectShutdownAck {
            acknowledged: true,
            shutdown_started: true,
            server_stopped: true,
            route_outcomes: vec![ShutdownControlRouteOutcome {
                route_name: "grid".to_string(),
                active_drained: true,
                closed_reason: "direct_ipc_shutdown".to_string(),
            }],
        };
        let payload = encode_shutdown_ack(&outcome).unwrap();
        assert_eq!(decode_shutdown_ack(&payload).unwrap(), outcome);
    }

    #[test]
    fn legacy_shutdown_ack_without_structured_outcome_is_rejected() {
        assert!(decode_shutdown_ack(&SHUTDOWN_ACK_BYTES).is_err());
    }

    #[test]
    fn shutdown_initiate_round_trip_is_single_byte_command() {
        let payload = encode_shutdown_initiate();
        assert_eq!(payload, SHUTDOWN_CLIENT_BYTES);
        decode_shutdown_initiate(&payload).unwrap();
    }

    #[test]
    fn shutdown_initiate_with_wait_budget_payload_is_rejected() {
        let mut payload = SHUTDOWN_CLIENT_BYTES.to_vec();
        payload.extend_from_slice(&450u64.to_le_bytes());
        assert!(decode_shutdown_initiate(&payload).is_err());
    }
}
