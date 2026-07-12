use assert_cmd::Command;

fn stdout_text(assert: &assert_cmd::assert::Assert) -> String {
    String::from_utf8_lossy(&assert.get_output().stdout).into_owned()
}

fn stderr_text(assert: &assert_cmd::assert::Assert) -> String {
    String::from_utf8_lossy(&assert.get_output().stderr).into_owned()
}

#[test]
fn help_lists_all_subcommands() {
    let mut command = Command::cargo_bin("engit").expect("engit binary should build");
    let assert = command.arg("--help").assert().success();
    let stdout = stdout_text(&assert);

    for subcommand in [
        "tag",
        "release",
        "status",
        "changelog",
        "cleanup",
        "web",
        "pull",
        "search",
        "publish",
        "publish-config",
    ] {
        assert!(
            stdout.contains(subcommand),
            "expected help to mention {subcommand}, got:\n{stdout}"
        );
    }
}

#[test]
fn missing_required_argument_returns_usage_error() {
    let mut command = Command::cargo_bin("engit").expect("engit binary should build");
    let assert = command
        .args(["publish-config", "studio"])
        .assert()
        .failure();
    let stderr = stderr_text(&assert);

    assert!(stderr.contains("Usage:"), "stderr was:\n{stderr}");
    assert!(
        stderr.contains("publish-config <NAME> <SOURCE>"),
        "stderr was:\n{stderr}"
    );
}

#[test]
fn tag_requires_exactly_one_bump_or_version_input() {
    let mut command = Command::cargo_bin("engit").expect("engit binary should build");
    let assert = command.arg("tag").assert().failure();
    let stderr = stderr_text(&assert);

    assert!(stderr.contains("Usage:"), "stderr was:\n{stderr}");
    assert!(stderr.contains("--major"), "stderr was:\n{stderr}");
    assert!(stderr.contains("--version"), "stderr was:\n{stderr}");
}

#[test]
fn publish_config_without_cfg_root_or_env_var_fails_with_expected_message() {
    let mut command = Command::cargo_bin("engit").expect("engit binary should build");
    let assert = command
        .args(["publish-config", "studio", "dummy.json"])
        .env_remove("ENVOY_CFG_ROOTS")
        .assert()
        .failure();
    let stderr = stderr_text(&assert);

    assert!(
        stderr.contains("Error: No --cfg-root specified and ENVOY_CFG_ROOTS is not set."),
        "stderr was:\n{stderr}"
    );
}
