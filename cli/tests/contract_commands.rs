use assert_cmd::Command;
use predicates::prelude::*;

fn valid_contract_json() -> String {
    r#"{
      "schema": "c-two.contract.v1",
      "crm": {
        "namespace": "test.contract",
        "name": "Portable",
        "version": "0.1.0"
      },
      "methods": [
        {
          "access": "write",
          "buffer": "view",
          "name": "echo",
          "parameters": [
            {
              "name": "value",
              "kind": "POSITIONAL_OR_KEYWORD",
              "default": {"kind": "missing"},
              "type": {
                "kind": "codec",
                "codec": {
                  "kind": "codec_ref",
                  "id": "org.example.codec",
                  "version": "1",
                  "schema_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                  "portable": true
                }
              }
            }
          ],
          "return": {
            "kind": "codec",
            "codec": {
              "kind": "codec_ref",
              "id": "org.example.codec",
              "version": "1",
              "schema_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
              "portable": true
            }
          },
          "wire": {
            "input": {
              "kind": "codec_ref",
              "id": "org.example.codec",
              "version": "1",
              "schema_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
              "portable": true
            },
            "output": {
              "kind": "codec_ref",
              "id": "org.example.codec",
              "version": "1",
              "schema_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
              "portable": true
            }
          }
        }
      ]
    }"#
    .to_string()
}

#[test]
fn contract_help_lists_validate() {
    let mut cmd = Command::cargo_bin("c3").unwrap();
    cmd.args(["contract", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("validate"));
}

#[test]
fn contract_validate_accepts_file() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("contract.json");
    std::fs::write(&path, valid_contract_json()).unwrap();

    let mut cmd = Command::cargo_bin("c3").unwrap();
    cmd.args(["contract", "validate", path.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("valid c-two.contract.v1"))
        .stdout(predicate::str::contains("sha256="));
}

#[test]
fn contract_validate_accepts_stdin() {
    let mut cmd = Command::cargo_bin("c3").unwrap();
    cmd.args(["contract", "validate", "-"])
        .write_stdin(valid_contract_json())
        .assert()
        .success()
        .stdout(predicate::str::contains("stdin: valid c-two.contract.v1"));
}

#[test]
fn contract_validate_rejects_pickle_wire_ref() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("contract.json");
    let invalid = valid_contract_json().replace(
        r#""input": {
              "kind": "codec_ref",
              "id": "org.example.codec",
              "version": "1",
              "schema_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
              "portable": true
            }"#,
        r#""input": {
              "family": "python-pickle-default",
              "kind": "builtin",
              "portable": false,
              "version": "pickle-protocol-5"
            }"#,
    );
    std::fs::write(&path, invalid).unwrap();

    let mut cmd = Command::cargo_bin("c3").unwrap();
    cmd.args(["contract", "validate", path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("python-pickle-default"));
}
