//! Querying the recovery executable must not initialize a second recovery.
use std::{os::unix::fs::symlink, process::Command};

#[test]
fn recovery_help_and_invalid_commands_do_not_start_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let recovery = dir.path().join("recovery");
    symlink(env!("CARGO_BIN_EXE_skyward"), &recovery).unwrap();
    let help = Command::new(&recovery).arg("--help").output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8(help.stdout).unwrap().contains("Commands:"));
    let invalid = Command::new(&recovery).arg("not-a-command").output().unwrap();
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8(invalid.stderr).unwrap().contains("unrecognized subcommand"));
}
