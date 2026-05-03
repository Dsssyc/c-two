pub(crate) fn normalize_relay_base_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

pub(crate) fn peer_endpoint_url(base_url: &str, endpoint: &str) -> String {
    let base = normalize_relay_base_url(base_url);
    let endpoint = endpoint.trim_start_matches('/');
    format!("{base}/{endpoint}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_endpoint_url_uses_exactly_one_separator() {
        assert_eq!(
            peer_endpoint_url(" http://relay-a:8080/ ", "/_peer/join"),
            "http://relay-a:8080/_peer/join"
        );
        assert_eq!(
            peer_endpoint_url("http://relay-a:8080", "_peer/join"),
            "http://relay-a:8080/_peer/join"
        );
    }

    #[test]
    fn normalize_relay_base_url_trims_trailing_slashes() {
        assert_eq!(
            normalize_relay_base_url(" http://relay-a:8080// "),
            "http://relay-a:8080"
        );
    }
}
