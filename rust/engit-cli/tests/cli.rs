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
fn publish_help_lists_bundle_stack_redirect_and_shim_subcommands() {
    let mut command = Command::cargo_bin("engit").expect("engit binary should build");
    let assert = command.args(["publish", "--help"]).assert().success();
    let stdout = stdout_text(&assert);

    assert!(stdout.contains("bundle"), "stdout was:\n{stdout}");
    assert!(stdout.contains("stack"), "stdout was:\n{stdout}");
    assert!(stdout.contains("redirect"), "stdout was:\n{stdout}");
    assert!(stdout.contains("shim"), "stdout was:\n{stdout}");
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
    // What the launcher on this system looks for.
    let executable = if cfg!(windows) { "envoy.exe" } else { "envoy" };
    std::fs::write(bin.join(executable), "").expect("executable is written");
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
fn a_version_is_held_to_the_systems_the_publisher_names() {
    let (temp, studio, source) = redirect_fixture();
    let root = temp.path().join("config");
    // The fixture has the executable of the system running the test, and no other.
    let (this_system, other_executable) = if cfg!(windows) {
        ("windows", "bin/envoy there")
    } else {
        ("linux", "bin/envoy.exe there")
    };
    let publish = |platform: &str| {
        Command::cargo_bin("engit")
            .expect("engit binary should build")
            .args(["publish", "redirect"])
            .arg(&source)
            .arg("--output")
            .arg(&root)
            .args(["--platform", platform])
            .env("ENGIT_TEST_STUDIO", &studio)
            .env_remove("ENVOY_CONFIG_PUBLISH_ROOT")
            .assert()
    };

    let refused = publish("all").failure();
    let stderr = stderr_text(&refused);
    assert!(stderr.contains(other_executable), "stderr was:\n{stderr}");
    assert_eq!(
        std::fs::read_dir(&root).expect("root reads").count(),
        0,
        "nothing may be written"
    );

    let accepted = publish(this_system).success();
    assert!(
        stdout_text(&accepted).contains("Published the redirect file."),
        "stdout was:\n{}",
        stdout_text(&accepted)
    );
}

#[cfg(unix)]
#[test]
fn a_published_file_is_readable_by_everyone_whatever_the_publishers_umask() {
    use std::os::unix::fs::PermissionsExt;

    let (temp, studio, source) = redirect_fixture();
    let root = temp.path().join("config");

    // Started through the shell so that the publisher's umask is the strictest common one.
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg("umask 077; exec \"$0\" \"$@\"")
        .arg(env!("CARGO_BIN_EXE_engit"))
        .args(["publish", "redirect"])
        .arg(&source)
        .arg("--output")
        .arg(&root)
        .env("ENGIT_TEST_STUDIO", &studio)
        .env_remove("ENVOY_CONFIG_PUBLISH_ROOT")
        .output()
        .expect("engit runs");
    assert!(
        output.status.success(),
        "stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mode = |path: &std::path::Path| {
        std::fs::metadata(path)
            .expect("metadata reads")
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(&root.join("redirect.yaml")), 0o644);
    let history = std::fs::read_dir(root.join("history"))
        .expect("history reads")
        .next()
        .expect("there is a history entry")
        .expect("entry reads")
        .path();
    assert_eq!(mode(&history), 0o644);
}

#[test]
fn publish_shim_without_a_root_or_env_var_fails_with_expected_message() {
    let mut command = Command::cargo_bin("engit").expect("engit binary should build");
    let assert = command
        .args(["publish", "shim", "dummy"])
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

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;

    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in files {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .expect("entry starts");
        writer.write_all(bytes).expect("entry is written");
    }
    writer.finish().expect("zip finishes").into_inner()
}

fn tar_gz_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    for (name, bytes) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, name, *bytes)
            .expect("file is added");
    }
    builder
        .into_inner()
        .expect("tar finishes")
        .finish()
        .expect("gzip finishes")
}

/// Add a version's launcher archive to a release folder, and write `SHA256SUMS`
/// for every launcher archive in it. Returns the platform of the archive.
///
/// The archive is for a platform other than this machine's, so that publishing
/// has no launcher to run: the files in it are not programs.
fn add_shim_archive(release: &std::path::Path, version: &str) -> &'static str {
    let (name, bytes, label) = if cfg!(windows) {
        (
            format!("envoy-shim-{version}-linux-x86_64-musl.tar.gz"),
            tar_gz_of(&[
                ("envoy", b"launcher"),
                ("install-envoy.sh", b"install"),
                ("uninstall-envoy.sh", b"uninstall"),
            ]),
            "linux-x86_64-musl",
        )
    } else {
        (
            format!("envoy-shim-{version}-windows-x86_64.zip"),
            zip_of(&[
                ("envoy.exe", b"launcher"),
                ("install-envoy.ps1", b"install"),
                ("uninstall-envoy.ps1", b"uninstall"),
            ]),
            "windows-x86_64",
        )
    };
    std::fs::write(release.join(&name), &bytes).expect("archive is written");

    let mut names: Vec<String> = std::fs::read_dir(release)
        .expect("release lists")
        .map(|item| {
            item.expect("entry reads")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.starts_with("envoy-shim-"))
        .collect();
    names.sort();
    let sums: String = names
        .iter()
        .map(|name| {
            let bytes = std::fs::read(release.join(name)).expect("archive reads");
            format!("{}  {name}\n", sha256_hex(&bytes))
        })
        .collect();
    std::fs::write(release.join("SHA256SUMS"), sums).expect("sums are written");
    label
}

/// A folder with a release's launcher archive and its sums, and a config root.
fn shim_fixture(
    version: &str,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    &'static str,
) {
    let temp = tempfile::tempdir().expect("tempdir is created");
    let release = temp.path().join("release");
    let root = temp.path().join("config");
    std::fs::create_dir_all(&release).expect("release is created");
    std::fs::create_dir_all(&root).expect("root is created");
    let label = add_shim_archive(&release, version);
    (temp, release, root, label)
}

fn publish_shim_command(release: &std::path::Path, root: &std::path::Path) -> Command {
    let mut command = Command::cargo_bin("engit").expect("engit binary should build");
    command
        .args(["publish", "shim"])
        .arg(release)
        .arg("--output")
        .arg(root)
        .env_remove("ENVOY_CONFIG_PUBLISH_ROOT")
        .env("ENVOY_CONFIG_PROD", "ignored");
    command
}

#[test]
fn a_launcher_release_is_published_and_the_pointer_moves() {
    let (_temp, release, root, label) = shim_fixture("v0.8.0");

    let published = publish_shim_command(&release, &root).assert().success();

    let stdout = stdout_text(&published);
    assert!(
        stdout.starts_with("Published the launcher v0.8.0."),
        "stdout was:\n{stdout}"
    );
    assert!(stdout.contains(label), "stdout was:\n{stdout}");
    let shim = root.join("shim");
    assert!(shim.join("v0.8.0").join(label).is_dir());
    assert_eq!(
        std::fs::read_to_string(shim.join("current.json")).expect("pointer reads"),
        "{\"schema\": 1, \"version\": \"v0.8.0\"}\n"
    );

    let again = publish_shim_command(&release, &root).assert().success();
    assert!(
        stdout_text(&again).starts_with("Nothing to publish"),
        "stdout was:\n{}",
        stdout_text(&again)
    );
}

#[test]
fn a_dry_run_writes_nothing_and_a_pre_release_needs_its_flag() {
    let (_temp, release, root, label) = shim_fixture("v0.9.0-rc.1");

    // A dry run is refused for what a real publish is refused.
    let refused = publish_shim_command(&release, &root)
        .arg("--dry-run")
        .assert()
        .failure();
    let stderr = stderr_text(&refused);
    assert!(
        stderr.contains("--allow-prerelease"),
        "stderr was:\n{stderr}"
    );
    assert!(stderr.contains("--no-pointer"), "stderr was:\n{stderr}");

    let dry = publish_shim_command(&release, &root)
        .args(["--dry-run", "--no-pointer"])
        .assert()
        .success();
    assert!(
        stdout_text(&dry).starts_with("Would publish the launcher v0.9.0-rc.1."),
        "stdout was:\n{}",
        stdout_text(&dry)
    );
    assert!(
        std::fs::read_dir(&root)
            .expect("root lists")
            .next()
            .is_none(),
        "a dry run wrote something"
    );

    // --no-pointer puts it on the share and leaves the pointer alone.
    let staged = publish_shim_command(&release, &root)
        .arg("--no-pointer")
        .assert()
        .success();
    assert!(
        stdout_text(&staged).contains("not moved"),
        "{}",
        stdout_text(&staged)
    );
    let shim = root.join("shim");
    assert!(shim.join("v0.9.0-rc.1").join(label).is_dir());
    assert!(!shim.join("current.json").exists());

    let moved = publish_shim_command(&release, &root)
        .arg("--allow-prerelease")
        .assert()
        .success();
    assert!(
        stdout_text(&moved).starts_with("Published the launcher v0.9.0-rc.1."),
        "stdout was:\n{}",
        stdout_text(&moved)
    );
    assert_eq!(
        std::fs::read_to_string(shim.join("current.json")).expect("pointer reads"),
        "{\"schema\": 1, \"version\": \"v0.9.0-rc.1\"}\n"
    );
}

#[test]
fn a_release_whose_archive_does_not_match_its_sums_is_refused() {
    let (_temp, release, root, _label) = shim_fixture("v0.8.0");
    let archive = std::fs::read_dir(&release)
        .expect("release lists")
        .map(|item| item.expect("entry reads").path())
        .find(|path| {
            path.extension()
                .is_some_and(|ext| ext == "zip" || ext == "gz")
        })
        .expect("there is an archive");
    let mut bytes = std::fs::read(&archive).expect("archive reads");
    bytes.push(0);
    std::fs::write(&archive, bytes).expect("archive is changed");

    let refused = publish_shim_command(&release, &root).assert().failure();

    let stderr = stderr_text(&refused);
    assert!(
        stderr.contains("does not match SHA256SUMS"),
        "stderr was:\n{stderr}"
    );
    assert!(
        std::fs::read_dir(&root)
            .expect("root lists")
            .next()
            .is_none(),
        "something was written"
    );
}

#[test]
fn a_folder_with_two_versions_needs_the_version_flag() {
    let (_temp, release, root, label) = shim_fixture("v0.8.0");
    add_shim_archive(&release, "v0.7.0");

    let refused = publish_shim_command(&release, &root).assert().failure();
    assert!(
        stderr_text(&refused).contains("--version"),
        "stderr was:\n{}",
        stderr_text(&refused)
    );

    let chosen = publish_shim_command(&release, &root)
        .args(["--version", "0.7.0"])
        .assert()
        .success();
    assert!(
        stdout_text(&chosen).starts_with("Published the launcher v0.7.0."),
        "stdout was:\n{}",
        stdout_text(&chosen)
    );
    assert!(root.join("shim").join("v0.7.0").join(label).is_dir());
    assert!(!root.join("shim").join("v0.8.0").exists());
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
