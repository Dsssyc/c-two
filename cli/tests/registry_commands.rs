use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn registry_help_lists_subcommands() {
    let mut cmd = Command::cargo_bin("c3").unwrap();
    cmd.args(["registry", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("list-routes"))
        .stdout(predicate::str::contains("resolve").not())
        .stdout(predicate::str::contains("peers"));
}

#[test]
fn registry_requires_relay_url() {
    let mut cmd = Command::cargo_bin("c3").unwrap();
    cmd.args(["registry", "list-routes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--relay"));
}

#[test]
fn registry_rejects_removed_name_only_resolve_subcommand() {
    let mut cmd = Command::cargo_bin("c3").unwrap();
    cmd.args([
        "registry",
        "resolve",
        "--relay",
        "http://127.0.0.1:8080",
        "grid",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains(
        "unrecognized subcommand 'resolve'",
    ));
}
