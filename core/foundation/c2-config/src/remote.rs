//! Protocol-neutral remote transport configuration.

pub const DEFAULT_REMOTE_PAYLOAD_CHUNK_SIZE: u64 = 1024 * 1024;
pub const MAX_REMOTE_PAYLOAD_CHUNK_SIZE: u64 = 128 * 1024 * 1024;

pub fn validate_remote_payload_chunk_size(value: u64) -> Result<(), String> {
    if value == 0 {
        return Err("must be > 0".into());
    }
    if value > MAX_REMOTE_PAYLOAD_CHUNK_SIZE {
        return Err(format!("must be <= {MAX_REMOTE_PAYLOAD_CHUNK_SIZE}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_payload_chunk_size_rejects_zero() {
        let err = validate_remote_payload_chunk_size(0)
            .expect_err("zero remote payload chunk size should fail");

        assert!(err.contains("> 0"));
    }

    #[test]
    fn remote_payload_chunk_size_rejects_values_above_hard_limit() {
        let err = validate_remote_payload_chunk_size(MAX_REMOTE_PAYLOAD_CHUNK_SIZE + 1)
            .expect_err("oversized remote payload chunk size should fail");

        assert!(err.contains("<="));
    }
}
