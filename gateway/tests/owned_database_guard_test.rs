//! Fake-Docker and child-command boundary tests for owned-database wrapping,
//! mutating-test URL guards, and the documented private CLI output recipe.
//!
//! These cases must not contact a real database or print secrets.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use support::{
    owned_database_url_violation, OWNED_DATABASE_HOST, OWNED_DATABASE_NAME,
    OWNED_DATABASE_PORT,
};

const INHERITED_SECRET: &str = "inherited-db-secret-do-not-print";
const OWNED_PASSWORD: &str = "owned/p@ss:word";
const REMOTE_URL: &str =
    "postgresql://postgres:inherited-db-secret-do-not-print@db.example.invalid:5432/postgres";
const UNRELATED_LOCAL_URL: &str =
    "postgresql://postgres:inherited-db-secret-do-not-print@127.0.0.1:5432/postgres";
const OUTPUT_SENTINEL: &str = "opmx_v1_TEST_SENTINEL_DO_NOT_PRINT";
const PRIVATE_OUTPUT_TEMPLATE: &str = "${TMPDIR:-/tmp}/opmux-key.XXXXXX";

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

    fn child(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn wrapper_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts/with-owned-database.sh")
}

fn install_fake_docker(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    fs::create_dir_all(&bin).expect("fake docker bin");
    let docker = bin.join("docker");
    fs::write(
        &docker,
        r#"#!/bin/sh
set -eu
if [ -n "${OPMUX_FAKE_DOCKER_LOG:-}" ]; then
  printf '%s\n' "$*" >> "$OPMUX_FAKE_DOCKER_LOG"
fi
cmd=${1:-}
[ -n "$cmd" ]
shift
expected=${OPMUX_FAKE_DOCKER_CONTAINER:-supabase_db_opmux-mvp-20260919}
case "$cmd" in
  port)
    if [ "${1:-}" != "$expected" ] || [ "${2:-}" != "5432/tcp" ]; then
      exit 1
    fi
    if [ "${OPMUX_FAKE_DOCKER_PORT_STATUS:-0}" -ne 0 ]; then
      exit "${OPMUX_FAKE_DOCKER_PORT_STATUS}"
    fi
    printf '%s\n' "${OPMUX_FAKE_DOCKER_PORT:-127.0.0.1:55432}"
    ;;
  exec)
    if [ "${1:-}" != "$expected" ]; then
      exit 1
    fi
    shift
    if [ "${1:-}" = "pg_isready" ]; then
      exit "${OPMUX_FAKE_DOCKER_READY_STATUS:-0}"
    fi
    if [ "${1:-}" = "printenv" ] && [ "${2:-}" = "POSTGRES_PASSWORD" ]; then
      printf '%s\n' "${OPMUX_FAKE_DOCKER_PASSWORD:-owned-pass}"
      exit 0
    fi
    exit 1
    ;;
  *)
    exit 127
    ;;
esac
"#,
    )
    .expect("write fake docker");
    let mut permissions = fs::metadata(&docker)
        .expect("docker metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&docker, permissions).expect("docker executable");
    bin
}

fn fake_path(bin: &Path) -> String {
    format!("{}:{}", bin.display(), std::env::var("PATH").expect("PATH"))
}

struct WrapperRun {
    status: i32,
    stdout: String,
    stderr: String,
}

fn run_wrapper(path_env: &str, extra_env: &[(&str, &str)], args: &[&str]) -> WrapperRun {
    let mut command = Command::new("bash");
    command.arg(wrapper_script());
    command.args(args);
    command.env("PATH", path_env);
    command.env(
        "OPMUX_FAKE_DOCKER_CONTAINER",
        "supabase_db_opmux-mvp-20260919",
    );
    command.env("OPMUX_FAKE_DOCKER_PASSWORD", OWNED_PASSWORD);
    command.env_remove("PGHOST");
    command.env_remove("PGHOSTADDR");
    command.env_remove("PGPORT");
    command.env_remove("PGOPTIONS");
    command.env_remove("PGPASSWORD");
    for (key, value) in extra_env {
        command.env(*key, *value);
    }
    let output = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run owned-database wrapper");
    WrapperRun {
        status: output.status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn assert_no_secrets(output: &str) {
    assert!(
        !output.contains(INHERITED_SECRET),
        "diagnostics must omit the inherited database secret"
    );
    assert!(
        !output.contains(OWNED_PASSWORD),
        "diagnostics must omit the owned database password"
    );
    assert!(
        !output.contains("postgresql://"),
        "diagnostics must omit connection URLs"
    );
    assert!(
        !output.contains(OUTPUT_SENTINEL),
        "diagnostics must omit credential sentinels"
    );
}

fn child_capture_args(
    url_file: &Path,
    ran_file: &Path,
    pghost_file: &Path,
) -> Vec<String> {
    vec![
        "sh".to_string(),
        "-c".to_string(),
        format!(
            "printf %s \"$DATABASE_URL\" > \"$1\"; printf %s \"${{PGHOST-}}\" > \"$2\"; : > \"$3\"",
        ),
        "child".to_string(),
        url_file.display().to_string(),
        pghost_file.display().to_string(),
        ran_file.display().to_string(),
    ]
}

#[test]
fn wrapper_replaces_inherited_remote_url_after_owned_checks() {
    let root = TempRoot::new("opmux-owned-wrapper");
    let bin = install_fake_docker(root.path());
    let log = root.child("docker.log");
    let url_file = root.child("child.url");
    let ran_file = root.child("child.ran");
    let pghost_file = root.child("child.pghost");
    let args = child_capture_args(&url_file, &ran_file, &pghost_file);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let run = run_wrapper(
        &fake_path(&bin),
        &[
            ("DATABASE_URL", REMOTE_URL),
            ("PGHOST", "db.example.invalid"),
            ("OPMUX_FAKE_DOCKER_LOG", log.to_str().expect("log path")),
            ("OPMUX_FAKE_DOCKER_PORT", "127.0.0.1:55432"),
            ("OPMUX_FAKE_DOCKER_PORT_STATUS", "0"),
            ("OPMUX_FAKE_DOCKER_READY_STATUS", "0"),
        ],
        &args,
    );
    assert_eq!(run.status, 0);
    assert!(
        ran_file.exists(),
        "child command must run after ownership checks"
    );
    let log_text = fs::read_to_string(&log).expect("docker log");
    assert!(
        log_text.contains("port supabase_db_opmux-mvp-20260919 5432/tcp"),
        "wrapper must inspect the owned container even when DATABASE_URL is set"
    );
    assert!(
        log_text.contains("pg_isready"),
        "wrapper must check readiness even when DATABASE_URL is set"
    );
    let child_url = fs::read_to_string(&url_file).expect("child url");
    let pghost = fs::read_to_string(&pghost_file).expect("child pghost");
    let replaced = child_url.contains("127.0.0.1:55432")
        && !child_url.contains("db.example.invalid")
        && !child_url.contains(INHERITED_SECRET)
        && !child_url.contains(OWNED_PASSWORD);
    assert!(
        replaced,
        "child DATABASE_URL must be the owned loopback URL without inherited or raw secrets"
    );
    assert!(
        pghost.is_empty(),
        "inherited libpq host overrides must not reach the child"
    );
    assert_no_secrets(&run.stdout);
    assert_no_secrets(&run.stderr);
}

#[test]
fn wrapper_replaces_unrelated_local_url() {
    let root = TempRoot::new("opmux-owned-wrapper-local");
    let bin = install_fake_docker(root.path());
    let url_file = root.child("child.url");
    let ran_file = root.child("child.ran");
    let pghost_file = root.child("child.pghost");
    let args = child_capture_args(&url_file, &ran_file, &pghost_file);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let run = run_wrapper(
        &fake_path(&bin),
        &[
            ("DATABASE_URL", UNRELATED_LOCAL_URL),
            ("OPMUX_FAKE_DOCKER_PORT", "127.0.0.1:55432"),
            ("OPMUX_FAKE_DOCKER_PORT_STATUS", "0"),
            ("OPMUX_FAKE_DOCKER_READY_STATUS", "0"),
        ],
        &args,
    );
    assert_eq!(run.status, 0);
    let child_url = fs::read_to_string(&url_file).expect("child url");
    assert!(
        child_url.contains("127.0.0.1:55432") && !child_url.contains(":5432/"),
        "unrelated local DATABASE_URL must be replaced with the owned port"
    );
    assert_no_secrets(&run.stdout);
    assert_no_secrets(&run.stderr);
}

#[test]
fn wrapper_blocks_missing_container_without_running_child() {
    let root = TempRoot::new("opmux-owned-wrapper-missing");
    let bin = install_fake_docker(root.path());
    let ran_file = root.child("child.ran");
    let run = run_wrapper(
        &fake_path(&bin),
        &[
            ("DATABASE_URL", REMOTE_URL),
            ("OPMUX_FAKE_DOCKER_PORT_STATUS", "1"),
        ],
        &[
            "sh",
            "-c",
            "printf ran > \"$1\"",
            "child",
            ran_file.to_str().expect("ran path"),
        ],
    );
    assert_ne!(run.status, 0);
    assert!(
        !ran_file.exists(),
        "child must not run when ownership fails"
    );
    assert!(
        run.stderr.contains("owned supabase") || run.stderr.contains("not running"),
        "failure diagnostics must identify the owned database setup"
    );
    assert_no_secrets(&run.stdout);
    assert_no_secrets(&run.stderr);
}

#[test]
fn wrapper_blocks_non_loopback_binding_without_running_child() {
    let root = TempRoot::new("opmux-owned-wrapper-bind");
    let bin = install_fake_docker(root.path());
    let ran_file = root.child("child.ran");
    let run = run_wrapper(
        &fake_path(&bin),
        &[
            ("DATABASE_URL", REMOTE_URL),
            ("OPMUX_FAKE_DOCKER_PORT", "0.0.0.0:55432"),
            ("OPMUX_FAKE_DOCKER_PORT_STATUS", "0"),
        ],
        &[
            "sh",
            "-c",
            "printf ran > \"$1\"",
            "child",
            ran_file.to_str().expect("ran path"),
        ],
    );
    assert_ne!(run.status, 0);
    assert!(
        !ran_file.exists(),
        "child must not run when binding is not loopback"
    );
    assert!(
        run.stderr.contains("127.0.0.1:55432"),
        "binding failures must name the required loopback address"
    );
    assert!(
        !run.stderr.contains("0.0.0.0:55432"),
        "binding failures should stay sanitized to the expected address"
    );
    assert_no_secrets(&run.stdout);
    assert_no_secrets(&run.stderr);
}

#[test]
fn wrapper_blocks_unreadiness_without_running_child() {
    let root = TempRoot::new("opmux-owned-wrapper-ready");
    let bin = install_fake_docker(root.path());
    let ran_file = root.child("child.ran");
    let run = run_wrapper(
        &fake_path(&bin),
        &[
            ("DATABASE_URL", REMOTE_URL),
            ("OPMUX_FAKE_DOCKER_PORT", "127.0.0.1:55432"),
            ("OPMUX_FAKE_DOCKER_PORT_STATUS", "0"),
            ("OPMUX_FAKE_DOCKER_READY_STATUS", "1"),
        ],
        &[
            "sh",
            "-c",
            "printf ran > \"$1\"",
            "child",
            ran_file.to_str().expect("ran path"),
        ],
    );
    assert_ne!(run.status, 0);
    assert!(
        !ran_file.exists(),
        "child must not run when the database is not ready"
    );
    assert!(
        run.stderr.contains("accepting connections") || run.stderr.contains("not ready"),
        "readiness failures must use a sanitized setup diagnostic"
    );
    assert_no_secrets(&run.stdout);
    assert_no_secrets(&run.stderr);
}

#[test]
fn mutating_helpers_reject_non_owned_effective_destinations_and_options() {
    let owned = "postgresql://postgres:x@127.0.0.1:55432/postgres?sslmode=disable";
    assert!(owned_database_url_violation(owned).is_none());
    assert_eq!(OWNED_DATABASE_HOST, "127.0.0.1");
    assert_eq!(OWNED_DATABASE_PORT, 55432);
    assert_eq!(OWNED_DATABASE_NAME, "postgres");

    let remote = owned_database_url_violation(REMOTE_URL);
    let unrelated = owned_database_url_violation(UNRELATED_LOCAL_URL);
    let localhost = owned_database_url_violation(
        "postgresql://postgres:x@localhost:55432/postgres?sslmode=disable",
    );
    let query_host = owned_database_url_violation(
        "postgresql://postgres:x@127.0.0.1:55432/postgres?host=db.example.invalid",
    );
    let hostaddr = owned_database_url_violation(
        "postgresql://postgres:x@127.0.0.1:55432/postgres?hostaddr=8.8.8.8",
    );
    let query_port = owned_database_url_violation(
        "postgresql://postgres:x@127.0.0.1:55432/postgres?port=5432",
    );
    let options = owned_database_url_violation(
        "postgresql://postgres:x@127.0.0.1:55432/postgres?options=-csearch_path=other",
    );
    let socket =
        owned_database_url_violation("postgresql://postgres@/postgres?host=/tmp");
    let other_db = owned_database_url_violation(
        "postgresql://postgres:x@127.0.0.1:55432/notpostgres?sslmode=disable",
    );
    assert!(remote.is_some(), "remote URLs must be rejected");
    assert!(
        unrelated.is_some(),
        "unrelated local ports must be rejected"
    );
    assert!(
        localhost.is_some(),
        "localhost is not the exact owned loopback host"
    );
    assert!(
        query_host.is_some(),
        "query host overrides must be rejected"
    );
    assert!(hostaddr.is_some(), "hostaddr overrides must be rejected");
    assert!(
        query_port.is_some(),
        "query port overrides must be rejected"
    );
    assert!(options.is_some(), "session options must be rejected");
    assert!(socket.is_some(), "unix sockets must be rejected");
    assert!(
        other_db.is_some(),
        "non-owned database names must be rejected"
    );
    for reason in [
        remote, unrelated, localhost, query_host, hostaddr, query_port, options, socket,
        other_db,
    ]
    .into_iter()
    .flatten()
    {
        assert!(
            !reason.contains(INHERITED_SECRET),
            "violation reasons must not echo secrets"
        );
        assert!(
            !reason.contains("db.example.invalid"),
            "violation reasons must not echo untrusted hosts"
        );
        assert!(
            !reason.contains("postgresql://"),
            "violation reasons must not echo URLs"
        );
    }
}

#[test]
fn help_documents_fresh_private_output_recipe() {
    let output = Command::new(env!("CARGO_BIN_EXE_opmux-admin"))
        .arg("--help")
        .env_remove("DATABASE_URL")
        .output()
        .expect("opmux-admin help");
    assert!(output.status.success());
    let help = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(help.contains("mktemp"));
    assert!(help.contains("opmux-key.XXXXXX"));
    assert!(help.contains("0600"));
    assert!(help.contains("umask"));
    assert!(help.contains("symlink") || help.contains("existing"));
    assert!(help.contains("once"));
    assert!(help.contains("opmux_operator"));
    assert!(help.contains("scripts/db-migrate.sh"));
    assert!(!help.contains("/tmp/acme-key.json"));
    assert!(!help.contains(OUTPUT_SENTINEL));
    assert!(!help.contains("opmx_v1_"));
}

#[test]
fn documented_private_output_recipe_is_safe_under_umask_022() {
    let root = TempRoot::new("opmux-private-output");
    let canary = root.child("canary.json");
    let existing = root.child("existing.json");
    let symlink = root.child("link.json");
    fs::write(&canary, "CANARY_DO_NOT_OVERWRITE").expect("canary");
    fs::write(&existing, "EXISTING_DO_NOT_OVERWRITE").expect("existing");
    std::os::unix::fs::symlink(&canary, &symlink).expect("symlink");
    let keyfile_path_file = root.child("keyfile.path");
    let script = format!(
        r#"
set -eu
umask 022
keyfile=$(mktemp "{template}")
chmod 600 "$keyfile"
printf '%s\n' "$keyfile" > "$1"
printf '%s\n' '{sentinel}' > "$keyfile"
"#,
        template = PRIVATE_OUTPUT_TEMPLATE,
        sentinel = OUTPUT_SENTINEL
    );
    let output = Command::new("sh")
        .arg("-c")
        .arg(script)
        .arg("recipe")
        .arg(&keyfile_path_file)
        .env("TMPDIR", root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run documented recipe");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success());
    assert!(
        !stdout.contains(OUTPUT_SENTINEL) && !stderr.contains(OUTPUT_SENTINEL),
        "recipe harness must not print the credential sentinel"
    );
    let keyfile = PathBuf::from(
        fs::read_to_string(&keyfile_path_file)
            .expect("keyfile path")
            .trim(),
    );
    assert!(keyfile.starts_with(root.path()));
    assert_ne!(keyfile, existing);
    assert_ne!(keyfile, symlink);
    assert_ne!(keyfile, canary);
    let mode = fs::metadata(&keyfile)
        .expect("keyfile metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "fresh private output must be mode 0600 under umask 022"
    );
    let written = fs::read_to_string(&keyfile).expect("keyfile");
    assert!(written.contains(OUTPUT_SENTINEL));
    assert_eq!(
        fs::read_to_string(&canary).expect("canary after"),
        "CANARY_DO_NOT_OVERWRITE"
    );
    assert_eq!(
        fs::read_to_string(&existing).expect("existing after"),
        "EXISTING_DO_NOT_OVERWRITE"
    );
    assert!(fs::symlink_metadata(&symlink)
        .expect("symlink metadata")
        .file_type()
        .is_symlink());
}

#[test]
fn docs_use_owned_wrapper_and_fresh_private_output() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let readme = fs::read_to_string(root.join("README.md")).expect("README");
    let runbook =
        fs::read_to_string(root.join("docs/OPERATIONS_RUNBOOK.md")).expect("runbook");
    for text in [&readme, &runbook] {
        assert!(text.contains("with-owned-database.sh bash scripts/db-migrate.sh"));
        assert!(text.contains("mktemp"));
        assert!(text.contains("opmux-key.XXXXXX"));
        assert!(text.contains("chmod 600"));
        assert!(!text.contains("> /tmp/acme-management.json"));
        assert!(!text.contains("> /tmp/acme-inference.json"));
    }
}
