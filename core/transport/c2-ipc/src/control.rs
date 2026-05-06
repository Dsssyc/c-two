use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use c2_config::validate_ipc_region_id;
use c2_wire::flags::{FLAG_RESPONSE, FLAG_SIGNAL};
use c2_wire::frame::{self, HEADER_SIZE};
use c2_wire::msg_type::{PING_BYTES, PONG_BYTES, SHUTDOWN_ACK_BYTES, SHUTDOWN_CLIENT_BYTES};

use crate::client::IpcError;

const IPC_SOCK_DIR: &str = "/tmp/c_two_ipc";

pub fn socket_path_from_ipc_address(address: &str) -> Result<PathBuf, IpcError> {
    let region = address
        .strip_prefix("ipc://")
        .ok_or_else(|| IpcError::Config(format!("invalid IPC address: {address}")))?;
    validate_ipc_region_id(region).map_err(IpcError::Config)?;
    Ok(PathBuf::from(IPC_SOCK_DIR).join(format!("{region}.sock")))
}

fn read_exact_or_none(stream: &mut UnixStream, len: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; len];
    match stream.read_exact(&mut buf) {
        Ok(()) => Some(buf),
        Err(_) => None,
    }
}

fn send_signal_and_read_reply(
    address: &str,
    timeout: Duration,
    signal: &[u8],
) -> Result<Option<(u32, Vec<u8>)>, IpcError> {
    let socket_path = socket_path_from_ipc_address(address)?;
    if !socket_path.exists() {
        return Ok(None);
    }

    let mut stream = match UnixStream::connect(&socket_path) {
        Ok(stream) => stream,
        Err(_) => return Ok(None),
    };
    if stream.set_read_timeout(Some(timeout)).is_err() {
        return Ok(None);
    }
    if stream.set_write_timeout(Some(timeout)).is_err() {
        return Ok(None);
    }

    let frame_bytes = frame::encode_frame(0, FLAG_SIGNAL, signal);
    if stream.write_all(&frame_bytes).is_err() {
        return Ok(None);
    }

    let header = match read_exact_or_none(&mut stream, HEADER_SIZE) {
        Some(header) => header,
        None => return Ok(None),
    };

    let (total_len, body) = match frame::decode_total_len(&header) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let (frame_header, payload_prefix) = match frame::decode_frame_body(body, total_len) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let payload_len = frame_header.payload_len();
    let mut payload = payload_prefix.to_vec();
    if payload.len() < payload_len {
        let remaining = payload_len - payload.len();
        match read_exact_or_none(&mut stream, remaining) {
            Some(tail) => payload.extend_from_slice(&tail),
            None => return Ok(None),
        }
    }
    Ok(Some((frame_header.flags, payload)))
}

fn is_valid_signal_reply(flags: u32, payload: &[u8], expected: &[u8]) -> bool {
    flags & FLAG_SIGNAL != 0 && flags & FLAG_RESPONSE != 0 && payload == expected
}

pub fn ping(address: &str, timeout: Duration) -> Result<bool, IpcError> {
    match send_signal_and_read_reply(address, timeout, &PING_BYTES)? {
        Some((flags, payload)) => Ok(is_valid_signal_reply(flags, &payload, &PONG_BYTES)),
        None => Ok(false),
    }
}

pub fn shutdown(address: &str, timeout: Duration) -> Result<bool, IpcError> {
    let socket_path = socket_path_from_ipc_address(address)?;
    if !socket_path.exists() {
        return Ok(true);
    }
    match send_signal_and_read_reply(address, timeout, &SHUTDOWN_CLIENT_BYTES)? {
        Some((flags, payload)) => Ok(is_valid_signal_reply(flags, &payload, &SHUTDOWN_ACK_BYTES)),
        None => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn socket_path_rejects_invalid_ipc_addresses() {
        for address in [
            "tcp://not-ipc",
            "ipc://",
            "ipc://../escape",
            "ipc://bad/name",
            "ipc://bad\\name",
            "ipc://.",
            "ipc://..",
            "ipc:// leading",
            "ipc://trailing ",
            "ipc://bad\nname",
        ] {
            let err = socket_path_from_ipc_address(address).expect_err("invalid address must fail");
            assert!(matches!(err, IpcError::Config(_)), "{address}: {err:?}");
        }
    }

    #[test]
    fn socket_path_accepts_plain_region() {
        let path = socket_path_from_ipc_address("ipc://unit-server").unwrap();
        assert_eq!(path, PathBuf::from("/tmp/c_two_ipc/unit-server.sock"));
    }

    #[test]
    fn ping_absent_socket_returns_false() {
        let address = "ipc://unit-control-absent";
        let path = socket_path_from_ipc_address(address).unwrap();
        let _ = fs::remove_file(path);
        let result = ping(address, Duration::from_millis(10)).unwrap();
        assert!(!result);
    }

    #[test]
    fn shutdown_absent_socket_returns_true() {
        let address = "ipc://unit-control-absent-shutdown";
        let path = socket_path_from_ipc_address(address).unwrap();
        let _ = fs::remove_file(path);
        let result = shutdown(address, Duration::from_millis(10)).unwrap();
        assert!(result);
    }

    #[test]
    fn ping_rejects_invalid_ipc_address() {
        let err = ping("tcp://not-ipc", Duration::from_millis(10))
            .expect_err("invalid address must fail");
        assert!(matches!(err, IpcError::Config(_)));
    }

    #[test]
    fn shutdown_rejects_invalid_ipc_address() {
        let err = shutdown("tcp://not-ipc", Duration::from_millis(10))
            .expect_err("invalid address must fail");
        assert!(matches!(err, IpcError::Config(_)));
    }
}
