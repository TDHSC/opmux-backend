//! Fake-Docker tests for `scripts/local-stack.sh` image selection, env
//! isolation, ownership-scoped cleanup, and the 605s stop ceiling.
//!
//! These cases must not contact a real Docker daemon, database, or provider.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serial_test::serial;

const INHERITED_SECRET: &str = "inherited-db-secret-do-not-print";
const OWNED_PASSWORD: &str = "owned/p@ss:word";
const INHERITED_URL: &str =
    "postgresql://postgres:inherited-db-secret-do-not-print@db.example.invalid:5432/postgres";
const SENTINEL_ENV: &str = "OPMUX_USER_SENTINEL=do-not-clobber\n";

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

struct RepoEnvGuard {
    path: PathBuf,
    previous: Option<Vec<u8>>,
}

impl RepoEnvGuard {
    fn write_opmux_local(contents: &str) -> Self {
        let path = repo_root().join(".env.opmux-local");
        let previous = fs::read(&path).ok();
        fs::write(&path, contents).expect("write sentinel env");
        Self { path, previous }
    }

    fn write_dotenv(contents: &str) -> Self {
        let path = repo_root().join(".env");
        let previous = fs::read(&path).ok();
        fs::write(&path, contents).expect("write project env");
        Self { path, previous }
    }
}

impl Drop for RepoEnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(bytes) => {
                let _ = fs::write(&self.path, bytes);
            }
            None => {
                let _ = fs::remove_file(&self.path);
            }
        }
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .expect("repo root")
}

fn stack_script() -> PathBuf {
    repo_root().join("scripts/local-stack.sh")
}

fn fake_docker_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/support/fake_local_stack_docker.py")
}

fn chmod_exec(path: &Path) {
    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod");
}

fn install_fakes(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    fs::create_dir_all(&bin).expect("fake bin");
    let docker = bin.join("docker");
    fs::copy(fake_docker_src(), &docker).expect("copy fake docker");
    chmod_exec(&docker);

    let lsof = bin.join("lsof");
    fs::write(&lsof, "#!/bin/sh\nexit 1\n").expect("fake lsof");
    chmod_exec(&lsof);

    let curl = bin.join("curl");
    fs::write(&curl, "#!/bin/sh\n# fake curl for wait_http\nexit 0\n")
        .expect("fake curl");
    chmod_exec(&curl);

    bin
}

fn fake_path(bin: &Path) -> String {
    format!("{}:{}", bin.display(), std::env::var("PATH").expect("PATH"))
}

struct ScriptRun {
    status: i32,
    stdout: String,
    stderr: String,
}

fn run_stack(
    path_env: &str,
    extra_env: &[(&str, &str)],
    unset: &[&str],
    args: &[&str],
) -> ScriptRun {
    let mut command = Command::new("bash");
    command.arg(stack_script());
    command.args(args);
    command.env("PATH", path_env);
    command.env("OPMUX_FAKE_DOCKER_PASSWORD", OWNED_PASSWORD);
    command.env("OPMUX_FAKE_DOCKER_PORT", "127.0.0.1:55432");
    command.env("OPMUX_FAKE_DOCKER_READY_STATUS", "0");
    command.env("OPMUX_FAKE_WORKDIR", repo_root().to_str().expect("root"));
    command.env(
        "OPMUX_FAKE_CONFIG_FILE",
        repo_root()
            .join("docker-compose.yml")
            .to_str()
            .expect("compose"),
    );
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
        .expect("run local-stack.sh");
    ScriptRun {
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
}

fn state_containers(state_dir: &Path) -> serde_json::Value {
    let raw = fs::read_to_string(state_dir.join("state.json")).expect("state");
    serde_json::from_str::<serde_json::Value>(&raw).expect("state json")
}

fn container_exists(state_dir: &Path, name: &str) -> bool {
    state_containers(state_dir)
        .get("containers")
        .and_then(|value| value.get(name))
        .is_some()
}

#[test]
fn down_succeeds_when_env_and_containers_are_absent() {
    let root = TempRoot::new("opmux-local-stack-absent");
    let bin = install_fakes(root.path());
    let state = root.child("state");
    let log = root.child("docker.log");
    let run = run_stack(
        &fake_path(&bin),
        &[
            ("OPMUX_FAKE_DOCKER_STATE", state.to_str().expect("state")),
            ("OPMUX_FAKE_DOCKER_LOG", log.to_str().expect("log")),
            ("OPMUX_FAKE_GATEWAY", "missing"),
            ("OPMUX_FAKE_SIMULATOR", "missing"),
        ],
        &[],
        &["down"],
    );
    assert_eq!(run.status, 0, "stderr={}", run.stderr);
    assert!(
        container_exists(&state, "supabase_db_opmux-mvp-20260919"),
        "absent cleanup must not touch the owned database record"
    );
    let log_text = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        !log_text.contains(" stop "),
        "absent containers must not be stopped"
    );
    assert!(
        !log_text.contains("compose down"),
        "cleanup must not use compose down"
    );
    assert_no_secrets(&run.stdout);
    assert_no_secrets(&run.stderr);
}

#[test]
fn down_is_idempotent_for_repeated_absent_cleanup() {
    let root = TempRoot::new("opmux-local-stack-repeat-absent");
    let bin = install_fakes(root.path());
    let state = root.child("state");
    let extra = [
        ("OPMUX_FAKE_DOCKER_STATE", state.to_str().expect("state")),
        ("OPMUX_FAKE_GATEWAY", "missing"),
        ("OPMUX_FAKE_SIMULATOR", "missing"),
    ];
    let first = run_stack(&fake_path(&bin), &extra, &[], &["down"]);
    let second = run_stack(&fake_path(&bin), &extra, &[], &["down"]);
    assert_eq!(first.status, 0, "stderr={}", first.stderr);
    assert_eq!(second.status, 0, "stderr={}", second.stderr);
}

#[test]
fn down_stops_owned_containers_without_compose_force_or_shared_resources() {
    let root = TempRoot::new("opmux-local-stack-owned-stop");
    let bin = install_fakes(root.path());
    let state = root.child("state");
    let log = root.child("docker.log");
    let run = run_stack(
        &fake_path(&bin),
        &[
            ("OPMUX_FAKE_DOCKER_STATE", state.to_str().expect("state")),
            ("OPMUX_FAKE_DOCKER_LOG", log.to_str().expect("log")),
            ("OPMUX_FAKE_GATEWAY", "owned"),
            ("OPMUX_FAKE_SIMULATOR", "owned"),
            ("SERVER_SHUTDOWN_TIMEOUT", "1"),
        ],
        &[],
        &["down"],
    );
    assert_eq!(run.status, 0, "stderr={}", run.stderr);
    assert!(
        !container_exists(&state, "opmux-local-gateway"),
        "owned gateway must be removed"
    );
    assert!(
        !container_exists(&state, "opmux-local-simulator"),
        "owned simulator must be removed"
    );
    assert!(
        container_exists(&state, "supabase_db_opmux-mvp-20260919"),
        "owned database container must remain"
    );
    let log_text = fs::read_to_string(&log).expect("log");
    assert!(
        log_text.contains("stop --time 605") || log_text.contains("STOP_TIMEOUT=605"),
        "docker stop must use the 605s ceiling, log={log_text}"
    );
    assert!(
        !log_text.contains("--time 8") && !log_text.contains("timeout 8"),
        "hardcoded 8s stop must not remain"
    );
    assert!(
        !log_text.contains("compose down"),
        "owned stop must not call compose down"
    );
    assert!(
        !log_text.contains("network rm") && !log_text.contains("volume"),
        "stop must not clean shared networks or volumes"
    );
    assert!(
        !log_text.contains(" rm -f ") && !log_text.contains(" rm --force "),
        "stop must not force-remove containers"
    );
    assert!(
        !log_text.contains("stop --time 605 supabase_db_opmux-mvp-20260919")
            && !log_text.contains(" rm supabase_db_opmux-mvp-20260919"),
        "database must not be stopped or removed"
    );
    assert!(
        run.stdout.contains("exited 0") || run.stderr.contains("exited 0"),
        "normal exit evidence must be captured before removal"
    );
    assert_no_secrets(&run.stdout);
    assert_no_secrets(&run.stderr);
}

#[test]
fn down_repeats_after_owned_containers_are_removed() {
    let root = TempRoot::new("opmux-local-stack-repeat-owned");
    let bin = install_fakes(root.path());
    let state = root.child("state");
    let extra = [
        ("OPMUX_FAKE_DOCKER_STATE", state.to_str().expect("state")),
        ("OPMUX_FAKE_GATEWAY", "owned"),
        ("OPMUX_FAKE_SIMULATOR", "owned"),
    ];
    let first = run_stack(&fake_path(&bin), &extra, &[], &["down"]);
    assert_eq!(first.status, 0, "stderr={}", first.stderr);
    let second = run_stack(&fake_path(&bin), &extra, &[], &["down"]);
    assert_eq!(second.status, 0, "stderr={}", second.stderr);
}

#[test]
fn down_refuses_foreign_project_without_stopping() {
    let root = TempRoot::new("opmux-local-stack-foreign");
    let bin = install_fakes(root.path());
    let state = root.child("state");
    let log = root.child("docker.log");
    let run = run_stack(
        &fake_path(&bin),
        &[
            ("OPMUX_FAKE_DOCKER_STATE", state.to_str().expect("state")),
            ("OPMUX_FAKE_DOCKER_LOG", log.to_str().expect("log")),
            ("OPMUX_FAKE_GATEWAY", "foreign"),
            ("OPMUX_FAKE_SIMULATOR", "missing"),
        ],
        &[],
        &["down"],
    );
    assert_ne!(run.status, 0, "foreign ownership must be refused");
    assert!(
        container_exists(&state, "opmux-local-gateway"),
        "foreign container must remain"
    );
    let log_text = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        !log_text.contains("stop --time") && !log_text.contains(" rm "),
        "foreign containers must not be stopped or removed: {log_text}"
    );
    assert!(
        run.stderr.contains("refusing") || run.stderr.contains("not the owned"),
        "refusal diagnostics must identify ownership, stderr={}",
        run.stderr
    );
    assert_no_secrets(&run.stdout);
    assert_no_secrets(&run.stderr);
}

#[test]
fn down_refuses_foreign_working_directory() {
    let root = TempRoot::new("opmux-local-stack-foreign-workdir");
    let bin = install_fakes(root.path());
    let state = root.child("state");
    let run = run_stack(
        &fake_path(&bin),
        &[
            ("OPMUX_FAKE_DOCKER_STATE", state.to_str().expect("state")),
            ("OPMUX_FAKE_GATEWAY", "foreign"),
            ("OPMUX_FAKE_SIMULATOR", "foreign"),
            ("OPMUX_FAKE_FOREIGN_PROJECT", "opmux-local"),
            ("OPMUX_FAKE_FOREIGN_WORKDIR", "/opt/other-repo"),
        ],
        &[],
        &["down"],
    );
    assert_ne!(run.status, 0);
    assert!(
        container_exists(&state, "opmux-local-gateway"),
        "foreign working-dir container must remain"
    );
}

#[test]
#[serial]
fn down_preserves_preexisting_env_opmux_local_and_does_not_need_it() {
    let _guard = RepoEnvGuard::write_opmux_local(SENTINEL_ENV);
    let root = TempRoot::new("opmux-local-stack-keep-env");
    let bin = install_fakes(root.path());
    let state = root.child("state");
    let run = run_stack(
        &fake_path(&bin),
        &[
            ("OPMUX_FAKE_DOCKER_STATE", state.to_str().expect("state")),
            ("OPMUX_FAKE_GATEWAY", "owned"),
            ("OPMUX_FAKE_SIMULATOR", "owned"),
        ],
        &[],
        &["down"],
    );
    assert_eq!(run.status, 0, "stderr={}", run.stderr);
    let kept = fs::read_to_string(repo_root().join(".env.opmux-local")).expect("env");
    assert_eq!(kept, SENTINEL_ENV);
}

#[test]
#[serial]
fn up_does_not_overwrite_preexisting_env_or_load_project_dotenv() {
    let _local = RepoEnvGuard::write_opmux_local(SENTINEL_ENV);
    let _dotenv = RepoEnvGuard::write_dotenv(
        "CONTAINER_IMAGE=from-project-env\nOPMUX_LOCAL_DATABASE_URL=postgresql://postgres:inherited-db-secret-do-not-print@db.example.invalid:5432/postgres\n",
    );
    let root = TempRoot::new("opmux-local-stack-up-env");
    let bin = install_fakes(root.path());
    let state = root.child("state");
    let capture = root.child("compose.json");
    let log = root.child("docker.log");
    let run = run_stack(
        &fake_path(&bin),
        &[
            ("OPMUX_FAKE_DOCKER_STATE", state.to_str().expect("state")),
            ("OPMUX_FAKE_DOCKER_LOG", log.to_str().expect("log")),
            ("OPMUX_FAKE_COMPOSE_CAPTURE", capture.to_str().expect("cap")),
            ("OPMUX_FAKE_GATEWAY", "missing"),
            ("OPMUX_FAKE_SIMULATOR", "missing"),
            (
                "OPMUX_FAKE_IMAGES",
                "opmux-gateway:mvp,opmux-simulator:local",
            ),
            ("OPMUX_LOCAL_BUILD", "0"),
        ],
        &[],
        &["up"],
    );
    assert_eq!(run.status, 0, "stderr={}", run.stderr);
    let kept = fs::read_to_string(repo_root().join(".env.opmux-local")).expect("env");
    assert_eq!(kept, SENTINEL_ENV);
    let capture_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&capture).expect("capture"))
            .expect("capture json");
    assert_eq!(
        capture_json["env_file_is_repo_dotenv"], false,
        "compose must not load project .env"
    );
    assert_eq!(
        capture_json["env_file_is_opmux_local"], false,
        "compose must not use .env.opmux-local as the generated file"
    );
    let env_file = capture_json["env_file"].as_str().unwrap_or_default();
    assert!(
        !env_file.is_empty()
            && !env_file.ends_with("/.env")
            && !env_file.ends_with("/.env.opmux-local"),
        "generated env file must be private, got {env_file}"
    );
    assert_no_secrets(&run.stdout);
    assert_no_secrets(&run.stderr);
}

#[test]
fn up_keeps_owned_database_url_authoritative_over_inherited_url() {
    let root = TempRoot::new("opmux-local-stack-url");
    let bin = install_fakes(root.path());
    let state = root.child("state");
    let capture = root.child("compose.json");
    let run = run_stack(
        &fake_path(&bin),
        &[
            ("OPMUX_FAKE_DOCKER_STATE", state.to_str().expect("state")),
            ("OPMUX_FAKE_COMPOSE_CAPTURE", capture.to_str().expect("cap")),
            ("OPMUX_FAKE_GATEWAY", "missing"),
            ("OPMUX_FAKE_SIMULATOR", "missing"),
            (
                "OPMUX_FAKE_IMAGES",
                "opmux-gateway:mvp,opmux-simulator:local",
            ),
            ("OPMUX_LOCAL_BUILD", "0"),
            ("OPMUX_LOCAL_DATABASE_URL", INHERITED_URL),
        ],
        &[],
        &["up"],
    );
    assert_eq!(run.status, 0, "stderr={}", run.stderr);
    let capture_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&capture).expect("capture"))
            .expect("capture json");
    assert_eq!(capture_json["database_url_has_owned_host"], true);
    assert_eq!(capture_json["database_url_has_inherited_host"], false);
    let host = capture_json["database_url_host"]
        .as_str()
        .unwrap_or_default();
    assert!(
        host.starts_with("supabase_db_opmux-mvp-20260919"),
        "owned injected URL must target the owned database, host={host}"
    );
    assert_no_secrets(&run.stdout);
    assert_no_secrets(&run.stderr);
}

#[test]
fn up_selects_container_image_for_inspect_and_compose() {
    let root = TempRoot::new("opmux-local-stack-image");
    let bin = install_fakes(root.path());
    let state = root.child("state");
    let capture = root.child("compose.json");
    let log = root.child("docker.log");
    let run = run_stack(
        &fake_path(&bin),
        &[
            ("OPMUX_FAKE_DOCKER_STATE", state.to_str().expect("state")),
            ("OPMUX_FAKE_DOCKER_LOG", log.to_str().expect("log")),
            ("OPMUX_FAKE_COMPOSE_CAPTURE", capture.to_str().expect("cap")),
            ("OPMUX_FAKE_GATEWAY", "missing"),
            ("OPMUX_FAKE_SIMULATOR", "missing"),
            (
                "OPMUX_FAKE_IMAGES",
                "opmux-gateway:check-tag,opmux-simulator:local",
            ),
            ("OPMUX_LOCAL_BUILD", "0"),
            ("CONTAINER_IMAGE", "opmux-gateway:check-tag"),
        ],
        &[],
        &["up"],
    );
    assert_eq!(run.status, 0, "stderr={}", run.stderr);
    let log_text = fs::read_to_string(&log).expect("log");
    assert!(
        log_text.contains("image inspect opmux-gateway:check-tag"),
        "inspect must use the selected tag, log={log_text}"
    );
    let capture_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&capture).expect("capture"))
            .expect("capture json");
    assert_eq!(capture_json["container_image"], "opmux-gateway:check-tag");
}

#[test]
fn up_fails_clearly_when_selected_image_is_missing_and_build_is_disabled() {
    let root = TempRoot::new("opmux-local-stack-missing-image");
    let bin = install_fakes(root.path());
    let state = root.child("state");
    let log = root.child("docker.log");
    let run = run_stack(
        &fake_path(&bin),
        &[
            ("OPMUX_FAKE_DOCKER_STATE", state.to_str().expect("state")),
            ("OPMUX_FAKE_DOCKER_LOG", log.to_str().expect("log")),
            ("OPMUX_FAKE_GATEWAY", "missing"),
            ("OPMUX_FAKE_SIMULATOR", "missing"),
            ("OPMUX_FAKE_IMAGES", "opmux-simulator:local"),
            ("OPMUX_LOCAL_BUILD", "0"),
            ("CONTAINER_IMAGE", "opmux-gateway:definitely-missing"),
        ],
        &[],
        &["up"],
    );
    assert_ne!(run.status, 0);
    assert!(
        run.stderr.contains("opmux-gateway:definitely-missing"),
        "missing-image errors must name the selected tag, stderr={}",
        run.stderr
    );
    assert!(
        run.stderr.contains("missing") || run.stderr.contains("build"),
        "missing-image errors must be actionable, stderr={}",
        run.stderr
    );
    let log_text = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        !log_text.contains("compose") || !log_text.contains(" up "),
        "missing no-build must not start the stack: {log_text}"
    );
    assert_no_secrets(&run.stdout);
    assert_no_secrets(&run.stderr);
}

#[test]
fn compose_gateway_image_uses_container_image_default() {
    let compose =
        fs::read_to_string(repo_root().join("docker-compose.yml")).expect("compose");
    assert!(
        compose.contains("image: ${CONTAINER_IMAGE:-opmux-gateway:mvp}"),
        "compose gateway image must interpolate CONTAINER_IMAGE"
    );
    let script = fs::read_to_string(stack_script()).expect("script");
    assert!(
        script.contains("stop --time 605"),
        "wrapper must use the 605s docker stop ceiling"
    );
    assert!(
        !script.contains("down --timeout 8") && !script.contains("--timeout 8"),
        "hardcoded 8s compose timeout must be removed"
    );
}
