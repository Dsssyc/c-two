use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use c2_config::validate_ipc_region_id;
use c2_wire::flags::{FLAG_RESPONSE, FLAG_SIGNAL};
use c2_wire::frame::{self, HEADER_SIZE};
use c2_wire::msg_type::{PING_BYTES, PONG_BYTES};
use c2_wire::shutdown_control::{DirectShutdownAck, decode_shutdown_ack, encode_shutdown_initiate};

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
    socket_path_from_ipc_address(address)?;
    let started = std::time::Instant::now();
    loop {
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            return Ok(false);
        }
        let remaining = timeout.saturating_sub(elapsed);
        let attempt_timeout = remaining.min(Duration::from_millis(100));
        match send_signal_and_read_reply(address, attempt_timeout, &PING_BYTES)? {
            Some((flags, payload)) => {
                return Ok(is_valid_signal_reply(flags, &payload, &PONG_BYTES));
            }
            None => {
                let sleep_for = remaining.min(Duration::from_millis(10));
                if sleep_for.is_zero() {
                    return Ok(false);
                }
                std::thread::sleep(sleep_for);
            }
        }
    }
}

pub fn shutdown(address: &str, timeout: Duration) -> Result<DirectShutdownAck, IpcError> {
    let socket_path = socket_path_from_ipc_address(address)?;
    let request = encode_shutdown_initiate();
    let started = std::time::Instant::now();
    loop {
        if !socket_path.exists() {
            return Ok(DirectShutdownAck {
                acknowledged: true,
                shutdown_started: false,
                server_stopped: true,
                route_outcomes: Vec::new(),
            });
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            return Ok(DirectShutdownAck {
                acknowledged: false,
                shutdown_started: false,
                server_stopped: false,
                route_outcomes: Vec::new(),
            });
        }
        let remaining = timeout.saturating_sub(elapsed);
        // Give the server enough time to accept and ack a draining duplicate
        // initiate; short retry loops can create stale connections faster than
        // the accept loop can drain them under load.
        let attempt_timeout = remaining.min(Duration::from_millis(500));
        match send_signal_and_read_reply(address, attempt_timeout, &request)? {
            Some((flags, payload)) => {
                if flags & FLAG_SIGNAL == 0 || flags & FLAG_RESPONSE == 0 {
                    return Ok(DirectShutdownAck {
                        acknowledged: false,
                        shutdown_started: false,
                        server_stopped: false,
                        route_outcomes: Vec::new(),
                    });
                }
                match decode_shutdown_ack(&payload) {
                    Ok(outcome) => return Ok(outcome),
                    Err(_) => {
                        return Ok(DirectShutdownAck {
                            acknowledged: false,
                            shutdown_started: false,
                            server_stopped: false,
                            route_outcomes: Vec::new(),
                        });
                    }
                }
            }
            None => {
                let sleep_for = remaining.min(Duration::from_millis(10));
                if sleep_for.is_zero() {
                    return Ok(DirectShutdownAck {
                        acknowledged: false,
                        shutdown_started: false,
                        server_stopped: false,
                        route_outcomes: Vec::new(),
                    });
                }
                std::thread::sleep(sleep_for);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use c2_wire::shutdown_control::encode_shutdown_ack;

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
    fn ping_retries_until_timeout_when_socket_appears_late() {
        let address = "ipc://unit-control-late-ping";
        let path = socket_path_from_ipc_address(address).unwrap();
        let _ = fs::remove_file(&path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let server_path = path.clone();
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            let listener = UnixListener::bind(&server_path).unwrap();
            let (mut stream, _) = listener.accept().unwrap();
            let mut header = [0u8; HEADER_SIZE];
            stream.read_exact(&mut header).unwrap();
            let (total_len, body) = frame::decode_total_len(&header).unwrap();
            let (frame_header, payload_prefix) = frame::decode_frame_body(body, total_len).unwrap();
            let payload_len = frame_header.payload_len();
            let mut payload = payload_prefix.to_vec();
            if payload.len() < payload_len {
                let mut tail = vec![0u8; payload_len - payload.len()];
                stream.read_exact(&mut tail).unwrap();
                payload.extend_from_slice(&tail);
            }
            assert_eq!(payload, PING_BYTES);
            let reply = frame::encode_frame(0, FLAG_SIGNAL | FLAG_RESPONSE, &PONG_BYTES);
            stream.write_all(&reply).unwrap();
        });

        let result = ping(address, Duration::from_millis(500)).unwrap();

        assert!(result);
        handle.join().unwrap();
        let _ = fs::remove_file(path);
    }

    #[test]
    fn shutdown_absent_socket_returns_true() {
        let address = "ipc://unit-control-absent-shutdown";
        let path = socket_path_from_ipc_address(address).unwrap();
        let _ = fs::remove_file(path);
        let result = shutdown(address, Duration::from_millis(10)).unwrap();
        assert!(result.acknowledged);
        assert!(!result.shutdown_started);
        assert!(result.server_stopped);
        assert!(result.route_outcomes.is_empty());
    }

    #[test]
    fn shutdown_live_socket_sends_single_byte_initiate_without_wait_budget() {
        let address = "ipc://unit-control-shutdown-initiate";
        let path = socket_path_from_ipc_address(address).unwrap();
        let _ = fs::remove_file(&path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let server_path = path.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let listener = UnixListener::bind(&server_path).unwrap();
            ready_tx.send(()).unwrap();
            let (mut stream, _) = listener.accept().unwrap();
            let mut header = [0u8; HEADER_SIZE];
            stream.read_exact(&mut header).unwrap();
            let (total_len, body) = frame::decode_total_len(&header).unwrap();
            let (frame_header, payload_prefix) = frame::decode_frame_body(body, total_len).unwrap();
            let payload_len = frame_header.payload_len();
            let mut payload = payload_prefix.to_vec();
            if payload.len() < payload_len {
                let mut tail = vec![0u8; payload_len - payload.len()];
                stream.read_exact(&mut tail).unwrap();
                payload.extend_from_slice(&tail);
            }
            assert_eq!(payload, encode_shutdown_initiate());
            let ack = DirectShutdownAck {
                acknowledged: true,
                shutdown_started: true,
                server_stopped: false,
                route_outcomes: Vec::new(),
            };
            let reply_payload = encode_shutdown_ack(&ack).unwrap();
            let reply = frame::encode_frame(0, FLAG_SIGNAL | FLAG_RESPONSE, &reply_payload);
            stream.write_all(&reply).unwrap();
        });

        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let result = shutdown(address, Duration::from_millis(500)).unwrap();

        assert!(result.acknowledged);
        assert!(result.shutdown_started);
        assert!(!result.server_stopped);
        assert!(result.route_outcomes.is_empty());
        handle.join().unwrap();
        let _ = fs::remove_file(path);
    }

    #[test]
    fn shutdown_retries_until_ack_deadline_while_socket_still_exists() {
        let address = "ipc://unit-control-shutdown-retry";
        let path = socket_path_from_ipc_address(address).unwrap();
        let _ = fs::remove_file(&path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let server_path = path.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let listener = UnixListener::bind(&server_path).unwrap();
            ready_tx.send(()).unwrap();

            let (mut first, _) = listener.accept().unwrap();
            let mut header = [0u8; HEADER_SIZE];
            first.read_exact(&mut header).unwrap();
            drop(first);

            let (mut second, _) = listener.accept().unwrap();
            let mut header = [0u8; HEADER_SIZE];
            second.read_exact(&mut header).unwrap();
            let (total_len, body) = frame::decode_total_len(&header).unwrap();
            let (frame_header, payload_prefix) = frame::decode_frame_body(body, total_len).unwrap();
            let payload_len = frame_header.payload_len();
            let mut payload = payload_prefix.to_vec();
            if payload.len() < payload_len {
                let mut tail = vec![0u8; payload_len - payload.len()];
                second.read_exact(&mut tail).unwrap();
                payload.extend_from_slice(&tail);
            }
            assert_eq!(payload, encode_shutdown_initiate());
            let ack = DirectShutdownAck {
                acknowledged: true,
                shutdown_started: true,
                server_stopped: false,
                route_outcomes: Vec::new(),
            };
            let reply_payload = encode_shutdown_ack(&ack).unwrap();
            let reply = frame::encode_frame(0, FLAG_SIGNAL | FLAG_RESPONSE, &reply_payload);
            second.write_all(&reply).unwrap();
        });

        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let result = shutdown(address, Duration::from_millis(500)).unwrap();

        assert!(result.acknowledged);
        assert!(result.shutdown_started);
        assert!(!result.server_stopped);
        assert!(result.route_outcomes.is_empty());
        handle.join().unwrap();
        let _ = fs::remove_file(path);
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
