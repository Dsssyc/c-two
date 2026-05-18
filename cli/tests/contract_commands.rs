use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::PathBuf;

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

fn invalid_pickle_contract_json() -> String {
    valid_contract_json().replace(
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
    )
}

#[cfg(unix)]
fn fake_python(tempdir: &tempfile::TempDir, payload: &str) -> PathBuf {
    let path = tempdir.path().join("python-fake");
    let script =
        format!("#!/bin/sh\ncat <<'C_TWO_CONTRACT_JSON'\n{payload}\nC_TWO_CONTRACT_JSON\n");
    std::fs::write(&path, script).unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

fn assert_valid_contract_file(path: &Path) {
    let payload = std::fs::read_to_string(path).unwrap();
    c2_contract::validate_portable_contract_descriptor_json(payload.as_bytes()).unwrap();
}

#[test]
fn contract_help_lists_descriptor_commands() {
    let mut cmd = Command::cargo_bin("c3").unwrap();
    cmd.args(["contract", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("codegen"))
        .stdout(predicate::str::contains("export"))
        .stdout(predicate::str::contains("infer"))
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
    std::fs::write(&path, invalid_pickle_contract_json()).unwrap();

    let mut cmd = Command::cargo_bin("c3").unwrap();
    cmd.args(["contract", "validate", path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("python-pickle-default"));
}

#[cfg(unix)]
#[test]
fn contract_export_wraps_python_and_validates_output() {
    let tempdir = tempfile::tempdir().unwrap();
    let python = fake_python(&tempdir, &valid_contract_json());

    let mut cmd = Command::cargo_bin("c3").unwrap();
    cmd.args([
        "contract",
        "export",
        "example.contracts:Grid",
        "--python",
        python.to_str().unwrap(),
        "--method",
        "echo",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains(r#""schema": "c-two.contract.v1""#));
}

#[cfg(unix)]
#[test]
fn contract_infer_wraps_python_and_writes_validated_output() {
    let tempdir = tempfile::tempdir().unwrap();
    let python = fake_python(&tempdir, &valid_contract_json());
    let output = tempdir.path().join("inferred.contract.json");

    let mut cmd = Command::cargo_bin("c3").unwrap();
    cmd.args([
        "contract",
        "infer",
        "example.resources:GridResource",
        "--python",
        python.to_str().unwrap(),
        "--namespace",
        "example.grid",
        "--version",
        "0.1.0",
        "--name",
        "Grid",
        "--method",
        "echo",
        "--out",
        output.to_str().unwrap(),
    ])
    .assert()
    .success()
    .stdout(predicate::str::is_empty());

    assert_valid_contract_file(&output);
}

#[cfg(unix)]
#[test]
fn contract_export_rejects_invalid_python_descriptor() {
    let tempdir = tempfile::tempdir().unwrap();
    let python = fake_python(&tempdir, &invalid_pickle_contract_json());

    let mut cmd = Command::cargo_bin("c3").unwrap();
    cmd.args([
        "contract",
        "export",
        "example.contracts:Grid",
        "--python",
        python.to_str().unwrap(),
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("python-pickle-default"));
}

#[test]
fn contract_codegen_typescript_writes_output() {
    let tempdir = tempfile::tempdir().unwrap();
    let contract = tempdir.path().join("contract.json");
    let output = tempdir.path().join("client.ts");
    std::fs::write(&contract, valid_contract_json()).unwrap();

    let mut cmd = Command::cargo_bin("c3").unwrap();
    cmd.args([
        "contract",
        "codegen",
        "typescript",
        contract.to_str().unwrap(),
        "--out",
        output.to_str().unwrap(),
    ])
    .assert()
    .success()
    .stdout(predicate::str::is_empty());

    let generated = std::fs::read_to_string(output).unwrap();
    assert!(generated.contains("export class PortableClient"));
    assert!(generated.contains("export const PORTABLE_CODEC_REQUIREMENTS"));
}

#[test]
fn contract_codegen_typescript_strict_rejects_external_codec() {
    let tempdir = tempfile::tempdir().unwrap();
    let contract = tempdir.path().join("contract.json");
    std::fs::write(&contract, valid_contract_json()).unwrap();

    let mut cmd = Command::cargo_bin("c3").unwrap();
    cmd.args([
        "contract",
        "codegen",
        "typescript",
        contract.to_str().unwrap(),
        "--strict-codecs",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains(
        "unsupported codec org.example.codec",
    ));
}
