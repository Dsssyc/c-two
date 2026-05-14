use std::convert::Infallible;

use bytes::Bytes;
use futures::stream;

use crate::client::HttpError;

pub(crate) fn validate_remote_payload_chunk_size(chunk_size: u64) -> Result<usize, HttpError> {
    c2_config::validate_remote_payload_chunk_size(chunk_size)
        .map_err(|reason| HttpError::InvalidInput(format!("remote_payload_chunk_size {reason}")))?;
    usize::try_from(chunk_size).map_err(|_| {
        HttpError::InvalidInput(format!(
            "remote_payload_chunk_size {chunk_size} exceeds this platform's addressable memory"
        ))
    })
}

#[cfg(test)]
pub(crate) fn payload_chunk_ranges(
    len: usize,
    chunk_size: usize,
) -> impl Iterator<Item = (usize, usize)> {
    (0..len).step_by(chunk_size).map(move |start| {
        let end = start.saturating_add(chunk_size).min(len);
        (start, end)
    })
}

pub(crate) fn reqwest_body_from_payload(
    data: &[u8],
    chunk_size: u64,
) -> Result<reqwest::Body, HttpError> {
    let chunk_size = validate_remote_payload_chunk_size(chunk_size)?;
    if data.len() <= chunk_size {
        return Ok(reqwest::Body::from(data.to_vec()));
    }
    let bytes = Bytes::copy_from_slice(data);
    let len = bytes.len();
    let chunks = stream::unfold((bytes, 0usize), move |(bytes, offset)| async move {
        if offset >= len {
            return None;
        }
        let end = offset.saturating_add(chunk_size).min(len);
        let chunk = bytes.slice(offset..end);
        Some((Ok::<Bytes, Infallible>(chunk), (bytes, end)))
    });
    Ok(reqwest::Body::wrap_stream(chunks))
}

#[cfg(feature = "relay")]
pub(crate) fn axum_body_from_payload(
    data: Vec<u8>,
    chunk_size: u64,
) -> Result<axum::body::Body, HttpError> {
    let chunk_size = validate_remote_payload_chunk_size(chunk_size)?;
    if data.len() <= chunk_size {
        return Ok(axum::body::Body::from(data));
    }
    let bytes = Bytes::from(data);
    let len = bytes.len();
    let chunks = stream::unfold((bytes, 0usize), move |(bytes, offset)| async move {
        if offset >= len {
            return None;
        }
        let end = offset.saturating_add(chunk_size).min(len);
        let chunk = bytes.slice(offset..end);
        Some((Ok::<Bytes, Infallible>(chunk), (bytes, end)))
    });
    Ok(axum::body::Body::from_stream(chunks))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_chunk_ranges_split_by_configured_size() {
        let ranges = payload_chunk_ranges(7, 3).collect::<Vec<_>>();

        assert_eq!(ranges, vec![(0, 3), (3, 6), (6, 7)]);
    }
}
