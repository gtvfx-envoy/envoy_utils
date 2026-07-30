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
        .args(["publish", "stack", "studio"])
        .assert()
        .failure();
    let stderr = stderr_text(&assert);

    assert!(stderr.contains("Usage:"), "stderr was:\n{stderr}");
    assert!(
        stderr.contains("publish stack <NAME> <SOURCE>"),
        "stderr was:\n{stderr}"
    );
}

#[test]
fn publish_help_lists_bundle_and_stack_subcommands() {
    let mut command = Command::cargo_bin("engit").expect("engit binary should build");
    let assert = command.args(["publish", "--help"]).assert().success();
    let stdout = stdout_text(&assert);

    assert!(stdout.contains("bundle"), "stdout was:\n{stdout}");
    assert!(stdout.contains("stack"), "stdout was:\n{stdout}");
}

#[test]
fn retired_publish_stack_command_is_rejected() {
    let mut command = Command::cargo_bin("engit").expect("engit binary should build");
    let assert = command.arg("publish-stack").assert().failure();
    let stderr = stderr_text(&assert);

    assert!(
        stderr.contains("unrecognized subcommand 'publish-stack'"),
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
fn publish_stack_without_stack_root_or_env_var_fails_with_expected_message() {
    let mut command = Command::cargo_bin("engit").expect("engit binary should build");
    let assert = command
        .args(["publish", "stack", "studio", "dummy.estack"])
        .env_remove("ENVOY_STACK_PUBLISH_ROOT")
        .env_remove("ENVOY_STACK_ROOTS")
        .assert()
        .failure();
    let stderr = stderr_text(&assert);

    assert!(
        stderr.contains(
            "Error: No --output specified and neither ENVOY_STACK_PUBLISH_ROOT nor \
ENVOY_STACK_ROOTS is set."
        ),
        "stderr was:\n{stderr}"
    );
}
