#[cfg(not(windows))]
use crate::utils::kill_homestar_daemon;
use crate::utils::{
    wait_for_socket_connection, wait_for_socket_connection_v6, ChildGuard, FileGuard, BIN_NAME,
};
use anyhow::Result;
use assert_cmd::prelude::*;
use once_cell::sync::Lazy;
use predicates::prelude::*;
use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

static BIN: Lazy<PathBuf> = Lazy::new(|| assert_cmd::cargo::cargo_bin(BIN_NAME));

#[test]
fn test_help_integration() -> Result<()> {
    Command::new(BIN.as_os_str())
        .arg("help")
        .assert()
        .success()
        .stdout(predicate::str::contains("start"))
        .stdout(predicate::str::contains("stop"))
        .stdout(predicate::str::contains("ping"))
        .stdout(predicate::str::contains("run"))
        .stdout(predicate::str::contains("help"))
        .stdout(predicate::str::contains("version"));

    Command::new(BIN.as_os_str())
        .arg("-h")
        .assert()
        .success()
        .stdout(predicate::str::contains("start"))
        .stdout(predicate::str::contains("stop"))
        .stdout(predicate::str::contains("ping"))
        .stdout(predicate::str::contains("run"))
        .stdout(predicate::str::contains("help"))
        .stdout(predicate::str::contains("version"));

    Ok(())
}

#[test]
fn test_version_integration() -> Result<()> {
    Command::new(BIN.as_os_str())
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "{} {}",
            BIN_NAME,
            env!("CARGO_PKG_VERSION")
        )));

    Ok(())
}

#[test]
fn test_server_not_running_integration() -> Result<()> {
    Command::new(BIN.as_os_str())
        .arg("ping")
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("Connection refused")
                .or(predicate::str::contains("No connection could be made")),
        );

    Command::new(BIN.as_os_str())
        .arg("ping")
        .arg("--host")
        .arg("::1")
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("Connection refused")
                .or(predicate::str::contains("No connection could be made")),
        );

    Command::new(BIN.as_os_str())
        .arg("stop")
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("Connection refused")
                .or(predicate::str::contains("server was already shutdown")
                    .or(predicate::str::contains("No connection could be made"))),
        );

    Ok(())
}

#[test]
fn test_server_integration() -> Result<()> {
    const DB: &str = "test_server_integration.db";
    let _db_guard = FileGuard::new(DB);

    Command::new(BIN.as_os_str())
        .arg("start")
        .arg("-db")
        .arg(DB)
        .assert()
        .failure();

    let homestar_proc = Command::new(BIN.as_os_str())
        .arg("start")
        .arg("-c")
        .arg("tests/fixtures/test_v6.toml")
        .arg("--db")
        .arg(DB)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    let _proc_guard = ChildGuard::new(homestar_proc);

    if wait_for_socket_connection_v6(9837, 100).is_err() {
        panic!("Homestar server/runtime failed to start in time");
    }

    Command::new(BIN.as_os_str())
        .arg("ping")
        .arg("--host")
        .arg("::1")
        .arg("-p")
        .arg("9837")
        .assert()
        .success()
        .stdout(predicate::str::contains("::1"))
        .stdout(predicate::str::contains("pong"));

    Command::new(BIN.as_os_str())
        .arg("ping")
        .arg("--host")
        .arg("::1")
        .arg("-p")
        .arg("9835")
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("Connection refused")
                .or(predicate::str::contains("No connection could be made")),
        );

    Ok(())
}

#[test]
fn test_workflow_run_integration() -> Result<()> {
    const DB: &str = "test_workflow_run_integration.db";
    let _db_guard = FileGuard::new(DB);

    let homestar_proc = Command::new(BIN.as_os_str())
        .arg("start")
        .arg("-c")
        .arg("tests/fixtures/test_workflow1.toml")
        .arg("--db")
        .arg(DB)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let _proc_guard = ChildGuard::new(homestar_proc);

    if wait_for_socket_connection_v6(9840, 100).is_err() {
        panic!("Homestar server/runtime failed to start in time");
    }

    Command::new(BIN.as_os_str())
        .arg("run")
        .arg("-p")
        .arg("9840")
        .arg("-w")
        .arg("tests/fixtures/test-workflow-add-one.json")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ipfs://bafybeidbyqpmztqkeot33lz4ev2ftjhqrnbh67go56tlgbf7qmy5xyzvg4",
        ))
        .stdout(predicate::str::contains("num_tasks"))
        .stdout(predicate::str::contains("progress_count"));

    // run another one of the same!
    Command::new(BIN.as_os_str())
        .arg("run")
        .arg("-p")
        .arg("9840")
        .arg("-w")
        .arg("tests/fixtures/test-workflow-add-one.json")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ipfs://bafybeidbyqpmztqkeot33lz4ev2ftjhqrnbh67go56tlgbf7qmy5xyzvg4",
        ))
        .stdout(predicate::str::contains("num_tasks"))
        .stdout(predicate::str::contains("progress_count"));

    Ok(())
}

#[test]
#[cfg(not(windows))]
fn test_daemon_integration() -> Result<()> {
    const DB: &str = "test_daemon_integration.db";
    let _db_guard = FileGuard::new(DB);

    Command::new(BIN.as_os_str())
        .arg("start")
        .arg("-c")
        .arg("tests/fixtures/test_v4.toml")
        .arg("-d")
        .env("DATABASE_URL", DB)
        .stdout(Stdio::piped())
        .assert()
        .success();

    if wait_for_socket_connection(9000, 100).is_err() {
        panic!("Homestar server/runtime failed to start in time");
    }

    Command::new(BIN.as_os_str())
        .arg("ping")
        .arg("--host")
        .arg("127.0.0.1")
        .arg("-p")
        .arg("9000")
        .assert()
        .success()
        .stdout(predicate::str::contains("127.0.0.1"))
        .stdout(predicate::str::contains("pong"));

    let _ = kill_homestar_daemon();
    Ok(())
}

#[test]
fn test_server_v4_integration() -> Result<()> {
    const DB: &str = "test_server_v4_integration.db";
    let _db_guard = FileGuard::new(DB);

    let homestar_proc = Command::new(BIN.as_os_str())
        .arg("start")
        .arg("-c")
        .arg("tests/fixtures/test_v4.toml")
        .arg("--db")
        .arg(DB)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let _proc_guard = ChildGuard::new(homestar_proc);

    if wait_for_socket_connection(9000, 1000).is_err() {
        panic!("Homestar server/runtime failed to start in time");
    }

    Command::new(BIN.as_os_str())
        .arg("ping")
        .arg("--host")
        .arg("127.0.0.1")
        .arg("-p")
        .arg("9000")
        .assert()
        .success()
        .stdout(predicate::str::contains("127.0.0.1"))
        .stdout(predicate::str::contains("pong"));

    Ok(())
}
