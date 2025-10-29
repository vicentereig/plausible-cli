use assert_cmd::prelude::*;
use insta::assert_snapshot;
use std::process::Command;
use tempfile::TempDir;

fn command(temp: &TempDir) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("plausible"));
    cmd.env("HOME", temp.path())
        .env("PLAUSIBLE_CLI_DISABLE_KEYRING", "1")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("APPDATA")
        .env_remove("LOCALAPPDATA");
    cmd
}

fn run_stdout(temp: &TempDir, args: &[&str]) -> String {
    let assert = command(temp).args(args).assert().success();
    String::from_utf8(assert.get_output().stdout.clone()).expect("utf8 stdout")
}

#[test]
fn accounts_list_empty_snapshot() {
    let temp = TempDir::new().expect("temp dir");
    let output = run_stdout(&temp, &["accounts", "list"]);
    assert_snapshot!("accounts_list_empty", output);
}

#[test]
fn accounts_list_json_after_add() {
    let temp = TempDir::new().expect("temp dir");
    command(&temp)
        .args([
            "accounts",
            "add",
            "--alias",
            "primary",
            "--api-key",
            "key-primary",
            "--label",
            "Primary",
        ])
        .assert()
        .success();

    command(&temp)
        .args([
            "accounts",
            "add",
            "--alias",
            "backup",
            "--api-key",
            "key-backup",
        ])
        .assert()
        .success();

    command(&temp)
        .args(["accounts", "budget", "--alias", "backup", "--daily", "321"])
        .assert()
        .success();

    let output = run_stdout(&temp, &["accounts", "list", "--output", "json"]);
    assert_snapshot!("accounts_list_json", output);
}

#[test]
fn queue_inspect_empty_json() {
    let temp = TempDir::new().expect("temp dir");
    command(&temp)
        .args(["accounts", "add", "--alias", "acct", "--api-key", "key"])
        .assert()
        .success();
    let output = run_stdout(&temp, &["queue", "inspect", "--output", "json"]);
    assert_snapshot!("queue_inspect_empty_json", output);
}

#[test]
fn events_template_json_snapshot() {
    let temp = TempDir::new().expect("temp dir");
    command(&temp)
        .args(["accounts", "add", "--alias", "acct", "--api-key", "key"])
        .assert()
        .success();
    let output = run_stdout(&temp, &["events", "template", "--output", "json"]);
    assert_snapshot!("events_template_json", output);
}
