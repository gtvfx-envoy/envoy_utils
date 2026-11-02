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
        "redirect",
        "dev",
        "release-train",
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
    let assert = command.args(["publish", "stack"]).assert().failure();
    let stderr = stderr_text(&assert);

    assert!(stderr.contains("Usage:"), "stderr was:\n{stderr}");
    assert!(
        stderr.contains("publish stack <SOURCE>"),
        "stderr was:\n{stderr}"
    );
}

#[test]
fn publish_help_lists_bundle_stack_and_redirect_subcommands() {
    let mut command = Command::cargo_bin("engit").expect("engit binary should build");
    let assert = command.args(["publish", "--help"]).assert().success();
    let stdout = stdout_text(&assert);

    assert!(stdout.contains("bundle"), "stdout was:\n{stdout}");
    assert!(stdout.contains("stack"), "stdout was:\n{stdout}");
    assert!(stdout.contains("redirect"), "stdout was:\n{stdout}");
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
fn publish_redirect_without_a_root_or_env_var_fails_with_expected_message() {
    let mut command = Command::cargo_bin("engit").expect("engit binary should build");
    let assert = command
        .args(["publish", "redirect", "dummy.yaml"])
        .env_remove("ENVOY_CONFIG_PUBLISH_ROOT")
        .env("ENVOY_CONFIG_PROD", "ignored")
        .assert()
        .failure();
    let stderr = stderr_text(&assert);

    assert!(
        stderr.contains("Error: No --output specified and ENVOY_CONFIG_PUBLISH_ROOT is not set."),
        "stderr was:\n{stderr}"
    );
}

const REDIRECT_FILE: &str = "\
schema: 1
sources:
  - ${ENGIT_TEST_STUDIO}/envoy/{version}
versions:
  default: v0.8.0
";

fn redirect_fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = tempfile::tempdir().expect("tempdir is created");
    let studio = temp.path().join("bndl");
    let bin = studio.join("envoy").join("v0.8.0").join("bin");
    std::fs::create_dir_all(&bin).expect("bin is created");
    std::fs::write(bin.join("envoy.exe"), "").expect("executable is written");
    let source = temp.path().join("redirect.yaml");
    std::fs::write(&source, REDIRECT_FILE).expect("source is written");
    std::fs::create_dir_all(temp.path().join("config")).expect("config root is created");
    (temp, studio, source)
}

#[test]
fn a_redirect_file_is_published_shown_listed_and_not_published_twice() {
    let (temp, studio, source) = redirect_fixture();
    let root = temp.path().join("config");
    let engit = || {
        let mut command = Command::cargo_bin("engit").expect("engit binary should build");
        command
            .env("ENGIT_TEST_STUDIO", &studio)
            .env_remove("ENVOY_CONFIG_PUBLISH_ROOT")
            .env_remove("ENVOY_CONFIG_PROD");
        command
    };

    let published = engit()
        .args(["publish", "redirect"])
        .arg(&source)
        .arg("--output")
        .arg(&root)
        .assert()
        .success();
    assert!(
        stdout_text(&published).contains("Published the redirect file."),
        "stdout was:\n{}",
        stdout_text(&published)
    );
    assert_eq!(
        std::fs::read_to_string(root.join("redirect.yaml")).expect("live file reads"),
        REDIRECT_FILE
    );

    let shown = engit()
        .args(["redirect", "show", "--user", "someone", "--root"])
        .arg(&root)
        .assert()
        .success();
    let shown = stdout_text(&shown);
    assert!(
        shown.contains("default  v0.8.0  (found at"),
        "stdout was:\n{shown}"
    );
    assert!(
        shown.contains("someone: group 'default' (not listed) -> v0.8.0"),
        "stdout was:\n{shown}"
    );

    let history = engit()
        .args(["redirect", "history", "--root"])
        .arg(&root)
        .assert()
        .success();
    assert!(
        stdout_text(&history).contains("(live)"),
        "stdout was:\n{}",
        stdout_text(&history)
    );

    let again = engit()
        .args(["publish", "redirect"])
        .arg(&source)
        .arg("--output")
        .arg(&root)
        .assert()
        .success();
    assert!(
        stdout_text(&again).contains("Nothing to publish"),
        "stdout was:\n{}",
        stdout_text(&again)
    );
}

#[test]
fn a_redirect_file_that_pins_a_missing_version_is_refused() {
    let (temp, studio, source) = redirect_fixture();
    let root = temp.path().join("config");
    std::fs::write(&source, REDIRECT_FILE.replace("v0.8.0", "v9.9.9")).expect("source is changed");

    let assert = Command::cargo_bin("engit")
        .expect("engit binary should build")
        .args(["publish", "redirect"])
        .arg(&source)
        .arg("--output")
        .arg(&root)
        .env("ENGIT_TEST_STUDIO", &studio)
        .assert()
        .failure();
    let stderr = stderr_text(&assert);

    assert!(stderr.contains("v9.9.9"), "stderr was:\n{stderr}");
    assert!(
        stderr.contains("--no-check-sources"),
        "stderr was:\n{stderr}"
    );
    assert_eq!(
        std::fs::read_dir(&root).expect("root reads").count(),
        0,
        "nothing may be written"
    );
}

#[test]
fn publish_stack_without_stack_root_or_env_var_fails_with_expected_message() {
    let mut command = Command::cargo_bin("engit").expect("engit binary should build");
    let assert = command
        .args(["publish", "stack", "dummy.estack"])
        .env_remove("ENVOY_STACK_PUBLISH_ROOT")
        .assert()
        .failure();
    let stderr = stderr_text(&assert);

    assert!(
        stderr.contains("Error: No --output specified and ENVOY_STACK_PUBLISH_ROOT is not set."),
        "stderr was:\n{stderr}"
    );
}
