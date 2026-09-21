//! Sandbox/fake-command regressions for `scripts/ci-local.sh` skip reporting
//! and image/runtime gate invocation. These cases must not run real Cargo,
//! Docker builds, or the owned database.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use support::isolate_provider_environment;

const INHERITED_PROVIDER_KEY: &str = "inherited-provider-key-do-not-use";

struct TempRoot {
    path: PathBuf,
}

impl TempRoot {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path).expect("temp root");
        let mut permissions = fs::metadata(&path).expect("temp metadata").permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).expect("temp mode");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct ScriptRun {
    status: i32,
    stdout: String,
    stderr: String,
    log: String,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn write_exec(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent dir");
    }
    fs::write(path, contents).expect("write exec");
    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod");
}

fn install_sandbox(root: &Path) -> (PathBuf, PathBuf) {
    let scripts = root.join("scripts");
    fs::create_dir_all(&scripts).expect("scripts dir");
    fs::copy(
        repo_root().join("scripts/ci-local.sh"),
        scripts.join("ci-local.sh"),
    )
    .expect("copy ci-local.sh");

    write_exec(
        &scripts.join("with-owned-database.sh"),
        r#"#!/bin/sh
set -eu
exec "$@"
"#,
    );
    write_exec(
        &scripts.join("with-safe-test-env.sh"),
        r#"#!/bin/sh
set -eu
exec "$@"
"#,
    );
    write_exec(
        &scripts.join("ci-setup-db.sh"),
        r#"#!/bin/sh
set -eu
printf 'ci-setup-db\n' >> "$OPMUX_FAKE_LOG"
"#,
    );
    write_exec(
        &scripts.join("check-startup.sh"),
        r#"#!/bin/sh
set -eu
printf 'check-startup\n' >> "$OPMUX_FAKE_LOG"
"#,
    );
    write_exec(
        &scripts.join("check-container.sh"),
        r#"#!/bin/sh
set -eu
printf 'check-container SKIP_IMAGE_BUILD=%s CONTAINER_IMAGE=%s\n' \
  "${SKIP_IMAGE_BUILD-}" "${CONTAINER_IMAGE-}" >> "$OPMUX_FAKE_LOG"
exit "${OPMUX_FAKE_CONTAINER_STATUS:-0}"
"#,
    );

    let bin = root.join("bin");
    fs::create_dir_all(&bin).expect("bin dir");
    write_exec(
        &bin.join("cargo"),
        r#"#!/bin/sh
set -eu
printf 'cargo %s\n' "$*" >> "$OPMUX_FAKE_LOG"
if [ "${1:-}" = "build" ]; then
  exit "${OPMUX_FAKE_CARGO_BUILD_STATUS:-0}"
fi
exit 0
"#,
    );
    write_exec(
        &bin.join("npm"),
        r#"#!/bin/sh
set -eu
printf 'npm %s\n' "$*" >> "$OPMUX_FAKE_LOG"
exit 0
"#,
    );
    write_exec(
        &bin.join("docker"),
        r#"#!/bin/sh
set -eu
printf 'docker %s\n' "$*" >> "$OPMUX_FAKE_LOG"
if [ "${1:-}" = "build" ]; then
  exit "${OPMUX_FAKE_DOCKER_BUILD_STATUS:-0}"
fi
exit 0
"#,
    );

    let log = root.join("invoked-gates.log");
    fs::write(&log, "").expect("log");
    (bin, log)
}

fn run_ci_local(
    root: &Path,
    bin: &Path,
    log: &Path,
    extra_env: &[(&str, &str)],
    unset: &[&str],
) -> ScriptRun {
    isolate_provider_environment();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").expect("PATH"));
    let mut command = Command::new("bash");
    command
        .arg(root.join("scripts/ci-local.sh"))
        .current_dir(root)
        .env("PATH", path)
        .env("OPMUX_FAKE_LOG", log)
        .env("OPENAI_API_KEY", INHERITED_PROVIDER_KEY)
        .env_remove("SKIP_IMAGE")
        .env_remove("SKIP_CONTAINER_CHECK")
        .env_remove("SKIP_IMAGE_BUILD")
        .env_remove("CONTAINER_IMAGE");
    for key in unset {
        command.env_remove(*key);
    }
    for (key, value) in extra_env {
        command.env(*key, *value);
    }
    let output = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run ci-local sandbox");
    ScriptRun {
        status: output.status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        log: fs::read_to_string(log).unwrap_or_default(),
    }
}

fn combined(run: &ScriptRun) -> String {
    format!("{}{}", run.stdout, run.stderr)
}

fn invoked_gates(log: &str) -> Vec<&str> {
    log.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect()
}

fn assert_no_secrets(output: &str) {
    assert!(
        !output.contains(INHERITED_PROVIDER_KEY),
        "diagnostics must omit inherited provider keys"
    );
    assert!(
        !output.contains("postgresql://"),
        "diagnostics must omit connection URLs"
    );
}

fn assert_not_full_or_partial_success(run: &ScriptRun) {
    let text = combined(run);
    assert!(
        !text.contains("local CI equivalent passed"),
        "failures must not report a complete local CI pass: {text}"
    );
    assert!(
        !text.contains("PARTIAL"),
        "failures must not report a partial-success result: {text}"
    );
    assert_no_secrets(&text);
}

const CORE_GATES: &[&str] = &[
    "ci-setup-db",
    "cargo test --workspace --locked --all-features -j 2 -- --test-threads=2",
    "cargo check --workspace --locked --all-targets --all-features -j 2",
    "cargo clippy --workspace --locked --all-targets --all-features -j 2 -- -D warnings",
    "cargo fmt --all -- --check",
    "npm run format:check",
    "cargo build -p gateway --locked -j 2",
    "check-startup",
];

fn assert_core_gates(gates: &[&str]) {
    assert!(
        gates.len() >= CORE_GATES.len(),
        "expected core gates, got {gates:?}"
    );
    assert_eq!(&gates[..CORE_GATES.len()], CORE_GATES);
}

fn assert_no_image_gates(log: &str) {
    assert!(
        !log.contains("docker build"),
        "image build must not run: {log}"
    );
    assert!(
        !log.contains("check-container"),
        "container runtime acceptance must not run: {log}"
    );
}

#[test]
fn default_full_run_requires_build_and_runtime_and_reports_pass() {
    let root = TempRoot::new("opmux-ci-local-full");
    let (bin, log) = install_sandbox(root.path());
    let run = run_ci_local(root.path(), &bin, &log, &[], &[]);
    let text = combined(&run);
    assert_eq!(run.status, 0, "full run should exit 0: {text}");
    assert!(
        text.contains("local CI equivalent passed"),
        "complete success requires actual image build and runtime acceptance: {text}"
    );
    assert!(
        !text.contains("PARTIAL"),
        "full pass must not be labeled partial: {text}"
    );
    let gates = invoked_gates(&run.log);
    assert_core_gates(&gates);
    assert_eq!(
        gates.as_slice(),
        [
            CORE_GATES[0],
            CORE_GATES[1],
            CORE_GATES[2],
            CORE_GATES[3],
            CORE_GATES[4],
            CORE_GATES[5],
            CORE_GATES[6],
            CORE_GATES[7],
            "docker build --file gateway/Dockerfile --tag opmux-gateway:mvp .",
            "check-container SKIP_IMAGE_BUILD=1 CONTAINER_IMAGE=opmux-gateway:mvp",
        ]
    );
    assert_no_secrets(&text);
}

#[test]
fn inherited_container_image_does_not_divert_runtime_acceptance_from_built_mvp() {
    let root = TempRoot::new("opmux-ci-local-inherited-image");
    let (bin, log) = install_sandbox(root.path());
    let run = run_ci_local(
        root.path(),
        &bin,
        &log,
        &[("CONTAINER_IMAGE", "opmux-gateway:old")],
        &[],
    );
    let text = combined(&run);
    assert_eq!(run.status, 0, "full run should exit 0: {text}");
    assert!(
        text.contains("local CI equivalent passed"),
        "complete success requires the built mvp image and runtime acceptance of that same tag: {text}"
    );
    assert!(
        !text.contains("PARTIAL"),
        "full pass must not be labeled partial: {text}"
    );
    let gates = invoked_gates(&run.log);
    assert_core_gates(&gates);
    assert_eq!(
        gates.as_slice(),
        [
            CORE_GATES[0],
            CORE_GATES[1],
            CORE_GATES[2],
            CORE_GATES[3],
            CORE_GATES[4],
            CORE_GATES[5],
            CORE_GATES[6],
            CORE_GATES[7],
            "docker build --file gateway/Dockerfile --tag opmux-gateway:mvp .",
            "check-container SKIP_IMAGE_BUILD=1 CONTAINER_IMAGE=opmux-gateway:mvp",
        ]
    );
    assert!(
        !run.log.contains("opmux-gateway:old"),
        "inherited CONTAINER_IMAGE must not reach docker build or the checker: {}",
        run.log
    );
    assert_no_secrets(&text);
}

#[test]
fn skip_image_exits_partial_and_names_skipped_gates() {
    let root = TempRoot::new("opmux-ci-local-skip-image");
    let (bin, log) = install_sandbox(root.path());
    let run = run_ci_local(root.path(), &bin, &log, &[("SKIP_IMAGE", "1")], &[]);
    let text = combined(&run);
    assert_eq!(run.status, 0, "development image skip may exit 0: {text}");
    assert!(
        text.contains("PARTIAL") && text.contains("not full acceptance"),
        "image skip must report partial non-acceptance: {text}"
    );
    assert!(
        text.contains("image build"),
        "partial output must name the skipped image build: {text}"
    );
    assert!(
        text.contains("container runtime acceptance"),
        "skipping image also skips runtime acceptance and must name it: {text}"
    );
    assert!(
        !text.contains("local CI equivalent passed"),
        "image skip must never report a full pass: {text}"
    );
    let gates = invoked_gates(&run.log);
    assert_core_gates(&gates);
    assert_no_image_gates(&run.log);
    assert_no_secrets(&text);
}

#[test]
fn skip_container_check_still_builds_and_reports_partial() {
    let root = TempRoot::new("opmux-ci-local-skip-container");
    let (bin, log) = install_sandbox(root.path());
    let run = run_ci_local(
        root.path(),
        &bin,
        &log,
        &[("SKIP_CONTAINER_CHECK", "1")],
        &[],
    );
    let text = combined(&run);
    assert_eq!(
        run.status, 0,
        "development container skip may exit 0: {text}"
    );
    assert!(
        text.contains("PARTIAL") && text.contains("not full acceptance"),
        "container skip must report partial non-acceptance: {text}"
    );
    assert!(
        text.contains("container runtime acceptance"),
        "partial output must name the skipped runtime gate: {text}"
    );
    assert!(
        !text.contains("local CI equivalent passed"),
        "container skip must never report a full pass: {text}"
    );
    let gates = invoked_gates(&run.log);
    assert_core_gates(&gates);
    assert!(
        run.log
            .contains("docker build --file gateway/Dockerfile --tag opmux-gateway:mvp ."),
        "container skip must still build the image: {}",
        run.log
    );
    assert!(
        !run.log.contains("check-container"),
        "container runtime acceptance must be skipped: {}",
        run.log
    );
    assert_no_secrets(&text);
}

#[test]
fn skip_both_image_gates_reports_partial_without_full_pass() {
    let root = TempRoot::new("opmux-ci-local-skip-both");
    let (bin, log) = install_sandbox(root.path());
    let run = run_ci_local(
        root.path(),
        &bin,
        &log,
        &[("SKIP_IMAGE", "1"), ("SKIP_CONTAINER_CHECK", "1")],
        &[],
    );
    let text = combined(&run);
    assert_eq!(run.status, 0, "development skips may exit 0: {text}");
    assert!(
        text.contains("PARTIAL") && text.contains("not full acceptance"),
        "both skips must report partial non-acceptance: {text}"
    );
    assert!(
        text.contains("image build"),
        "must name image build: {text}"
    );
    assert!(
        text.contains("container runtime acceptance"),
        "must name container runtime acceptance: {text}"
    );
    assert!(
        !text.contains("local CI equivalent passed"),
        "both skips must never report a full pass: {text}"
    );
    let gates = invoked_gates(&run.log);
    assert_core_gates(&gates);
    assert_no_image_gates(&run.log);
    assert_no_secrets(&text);
}

#[test]
fn image_build_failure_is_not_complete_or_partial_success() {
    let root = TempRoot::new("opmux-ci-local-build-fail");
    let (bin, log) = install_sandbox(root.path());
    let run = run_ci_local(
        root.path(),
        &bin,
        &log,
        &[("OPMUX_FAKE_DOCKER_BUILD_STATUS", "1")],
        &[],
    );
    assert_ne!(run.status, 0, "image build failure must be nonzero");
    assert_not_full_or_partial_success(&run);
    assert!(
        run.log
            .contains("docker build --file gateway/Dockerfile --tag opmux-gateway:mvp ."),
        "failed build must still have been invoked: {}",
        run.log
    );
    assert!(
        !run.log.contains("check-container"),
        "runtime acceptance must not run after a failed build: {}",
        run.log
    );
}

#[test]
fn runtime_acceptance_failure_is_not_complete_or_partial_success() {
    let root = TempRoot::new("opmux-ci-local-runtime-fail");
    let (bin, log) = install_sandbox(root.path());
    let run = run_ci_local(
        root.path(),
        &bin,
        &log,
        &[("OPMUX_FAKE_CONTAINER_STATUS", "1")],
        &[],
    );
    assert_ne!(run.status, 0, "runtime acceptance failure must be nonzero");
    assert_not_full_or_partial_success(&run);
    assert!(
        run.log.contains(
            "check-container SKIP_IMAGE_BUILD=1 CONTAINER_IMAGE=opmux-gateway:mvp"
        ),
        "runtime gate must have been invoked after this script's build: {}",
        run.log
    );
}
