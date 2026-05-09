//! V2 call/reply control-plane codec.
//!
//! In wire v2, routing metadata travels in the UDS inline frame (small),
//! while the SHM buddy block contains only pure serialized payload.
//!
//! ## V2 Call Control
//!
//! ```text
//! [1B name_len][route_name UTF-8][2B method_idx LE]
//! ```
//!
//! `name_len=0` means "use connection default route".
//!
//! ## V2 Reply Control
//!
//! ```text
//! [1B status]
//! if status == STATUS_ERROR (0x01):
//!     [4B error_len LE][error_bytes]
//! if status == STATUS_ROUTE_NOT_FOUND (0x02):
//!     [4B route_len LE][route_name UTF-8]
//! ```

use crate::frame::DecodeError;

/// Wire encoding failed because the requested control frame cannot be
/// represented by the protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    FieldTooLong {
        field: &'static str,
        max: usize,
        actual: usize,
    },
    InvalidText {
        field: &'static str,
        reason: String,
    },
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FieldTooLong { field, max, actual } => {
                write!(f, "{field} is too long: {actual} bytes > {max}")
            }
            Self::InvalidText { field, reason } => {
                write!(f, "{field} is invalid: {reason}")
            }
        }
    }
}

impl std::error::Error for EncodeError {}

/// Reply status: success — result data follows (inline or in buddy SHM).
pub const STATUS_SUCCESS: u8 = 0x00;

/// Reply status: error — error data follows inline.
pub const STATUS_ERROR: u8 = 0x01;

/// Reply status: requested route is no longer present on the IPC server.
pub const STATUS_ROUTE_NOT_FOUND: u8 = 0x02;

pub const MAX_CALL_ROUTE_NAME_BYTES: usize = u8::MAX as usize;

// ── V2 Call Control ──────────────────────────────────────────────────────

/// Decoded v2 call control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallControl {
    /// Route name (empty string = default route).
    pub route_name: String,
    /// Method index within the route's method table.
    pub method_idx: u16,
}

/// Encode v2 call control: `[1B name_len][route UTF-8][2B method_idx LE]`.
pub fn encode_call_control(route_name: &str, method_idx: u16) -> Result<Vec<u8>, EncodeError> {
    let name_bytes = route_name.as_bytes();
    if name_bytes.len() > MAX_CALL_ROUTE_NAME_BYTES {
        return Err(EncodeError::FieldTooLong {
            field: "route_name",
            max: MAX_CALL_ROUTE_NAME_BYTES,
            actual: name_bytes.len(),
        });
    }
    let mut buf = Vec::with_capacity(1 + name_bytes.len() + 2);
    buf.push(name_bytes.len() as u8);
    buf.extend_from_slice(name_bytes);
    buf.extend_from_slice(&method_idx.to_le_bytes());
    Ok(buf)
}

/// Encode v2 call control directly into a buffer at `offset`.
///
/// Returns the number of bytes written (1 + name_len + 2).
pub fn encode_call_control_into(
    buf: &mut [u8],
    offset: usize,
    route_name: &str,
    method_idx: u16,
) -> Result<usize, EncodeError> {
    let name_bytes = route_name.as_bytes();
    if name_bytes.len() > MAX_CALL_ROUTE_NAME_BYTES {
        return Err(EncodeError::FieldTooLong {
            field: "route_name",
            max: MAX_CALL_ROUTE_NAME_BYTES,
            actual: name_bytes.len(),
        });
    }
    let len = 1 + name_bytes.len() + 2;
    buf[offset] = name_bytes.len() as u8;
    buf[offset + 1..offset + 1 + name_bytes.len()].copy_from_slice(name_bytes);
    let idx_off = offset + 1 + name_bytes.len();
    buf[idx_off..idx_off + 2].copy_from_slice(&method_idx.to_le_bytes());
    Ok(len)
}

/// Decode v2 call control from `buf[offset..]`.
///
/// Returns `(control, bytes_consumed)`.
pub fn decode_call_control(buf: &[u8], offset: usize) -> Result<(CallControl, usize), DecodeError> {
    let remaining = buf.len().saturating_sub(offset);
    if remaining < 3 {
        return Err(DecodeError::BufferTooShort {
            need: 3,
            have: remaining,
        });
    }
    let name_len = buf[offset] as usize;
    let needed = 1 + name_len + 2;
    if remaining < needed {
        return Err(DecodeError::Truncated {
            field: "call control",
            need: needed,
            have: remaining,
        });
    }
    let name_start = offset + 1;
    let route_name = if name_len > 0 {
        core::str::from_utf8(&buf[name_start..name_start + name_len])
            .map_err(|_| DecodeError::Utf8Error)?
            .into()
    } else {
        String::new()
    };
    let idx_start = name_start + name_len;
    let method_idx = u16::from_le_bytes([buf[idx_start], buf[idx_start + 1]]);
    Ok((
        CallControl {
            route_name,
            method_idx,
        },
        needed,
    ))
}

// ── V2 Reply Control ─────────────────────────────────────────────────────

/// Decoded v2 reply control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyControl {
    /// Success — result data follows (inline or buddy SHM).
    Success,
    /// System error — route no longer exists on the upstream IPC server.
    RouteNotFound(String),
    /// Error — error bytes follow inline.
    Error(Vec<u8>),
}

/// Encode v2 reply control.
pub fn encode_reply_control(ctrl: &ReplyControl) -> Vec<u8> {
    match ctrl {
        ReplyControl::Success => {
            vec![STATUS_SUCCESS]
        }
        ReplyControl::RouteNotFound(route_name) => {
            let route_bytes = route_name.as_bytes();
            let mut buf = Vec::with_capacity(1 + 4 + route_bytes.len());
            buf.push(STATUS_ROUTE_NOT_FOUND);
            buf.extend_from_slice(&(route_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(route_bytes);
            buf
        }
        ReplyControl::Error(err_data) => {
            let mut buf = Vec::with_capacity(1 + 4 + err_data.len());
            buf.push(STATUS_ERROR);
            buf.extend_from_slice(&(err_data.len() as u32).to_le_bytes());
            buf.extend_from_slice(err_data);
            buf
        }
    }
}

/// Decode v2 reply control from `buf[offset..]`.
///
/// Returns `(control, bytes_consumed)`.
pub fn decode_reply_control(
    buf: &[u8],
    offset: usize,
) -> Result<(ReplyControl, usize), DecodeError> {
    let remaining = buf.len().saturating_sub(offset);
    if remaining < 1 {
        return Err(DecodeError::BufferTooShort { need: 1, have: 0 });
    }
    let status = buf[offset];
    match status {
        STATUS_SUCCESS => Ok((ReplyControl::Success, 1)),
        STATUS_ERROR => {
            if remaining < 5 {
                return Err(DecodeError::Truncated {
                    field: "reply control error_len",
                    need: 5,
                    have: remaining,
                });
            }
            let err_len = u32::from_le_bytes([
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
                buf[offset + 4],
            ]) as usize;
            let total = 5 + err_len;
            if remaining < total {
                return Err(DecodeError::Truncated {
                    field: "reply control error_data",
                    need: total,
                    have: remaining,
                });
            }
            let err_data = buf[offset + 5..offset + 5 + err_len].to_vec();
            Ok((ReplyControl::Error(err_data), total))
        }
        STATUS_ROUTE_NOT_FOUND => {
            if remaining < 5 {
                return Err(DecodeError::Truncated {
                    field: "reply control route_len",
                    need: 5,
                    have: remaining,
                });
            }
            let route_len = u32::from_le_bytes([
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
                buf[offset + 4],
            ]) as usize;
            let total = 5 + route_len;
            if remaining < total {
                return Err(DecodeError::Truncated {
                    field: "reply control route_name",
                    need: total,
                    have: remaining,
                });
            }
            let route_name = core::str::from_utf8(&buf[offset + 5..offset + 5 + route_len])
                .map_err(|_| DecodeError::Utf8Error)?
                .to_string();
            Ok((ReplyControl::RouteNotFound(route_name), total))
        }
        _ => Err(DecodeError::InvalidValue {
            field: "reply status",
            value: status as u64,
        }),
    }
}
