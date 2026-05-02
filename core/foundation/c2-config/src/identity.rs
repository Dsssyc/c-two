pub fn validate_ipc_region_id(region: &str) -> Result<(), String> {
    validate_id("region ID", region)
}

pub fn validate_server_id(server_id: &str) -> Result<(), String> {
    validate_id("server_id", server_id)
}

fn validate_id(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{label} cannot be empty"));
    }
    if value.trim() != value {
        return Err(format!(
            "{label} cannot contain leading or trailing whitespace"
        ));
    }
    if value == "." || value == ".." || value.contains('/') || value.contains('\\') {
        return Err(format!("{label} cannot contain path separators"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{label} cannot contain control characters"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_region_id_rejects_path_like_values() {
        for value in [
            "",
            "../escape",
            "bad/name",
            "bad\\name",
            ".",
            "..",
            " leading",
            "trailing ",
            "bad\nname",
        ] {
            assert!(
                validate_ipc_region_id(value).is_err(),
                "region should be rejected: {value:?}"
            );
        }
    }

    #[test]
    fn server_id_rejects_path_like_values() {
        for value in [
            "",
            "../escape",
            "bad/name",
            "bad\\name",
            ".",
            "..",
            " leading",
            "trailing ",
            "bad\nname",
        ] {
            assert!(
                validate_server_id(value).is_err(),
                "server_id should be rejected: {value:?}"
            );
        }
    }

    #[test]
    fn plain_ids_are_accepted() {
        validate_ipc_region_id("unit-server").unwrap();
        validate_server_id("unit-server").unwrap();
    }
}
