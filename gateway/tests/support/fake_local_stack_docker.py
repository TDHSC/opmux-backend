#!/usr/bin/env python3
"""Fake Docker CLI for local-stack.sh ownership, image, and stop tests.

Does not talk to a real Docker daemon, database, or network. Command lines
and selected interpolation values are recorded; secrets are not printed.
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path


OWNED_PROJECT = "opmux-local"
DB_CONTAINER = "supabase_db_opmux-mvp-20260919"
GATEWAY = "opmux-local-gateway"
SIMULATOR = "opmux-local-simulator"
NETWORK = "opmux-mvp-20260919-loopback"


def state_dir() -> Path:
    path = Path(os.environ["OPMUX_FAKE_DOCKER_STATE"])
    path.mkdir(parents=True, exist_ok=True)
    return path


def state_file() -> Path:
    return state_dir() / "state.json"


def log_path() -> Path | None:
    raw = os.environ.get("OPMUX_FAKE_DOCKER_LOG")
    return Path(raw) if raw else None


def capture_path() -> Path | None:
    raw = os.environ.get("OPMUX_FAKE_COMPOSE_CAPTURE")
    return Path(raw) if raw else None


def append_log(line: str) -> None:
    path = log_path()
    if path is None:
        return
    with path.open("a", encoding="utf-8") as handle:
        handle.write(line + "\n")


def seed_container(kind: str, service: str, running: bool) -> dict | None:
    if kind == "missing":
        return None
    workdir = os.environ.get("OPMUX_FAKE_WORKDIR", "/opt/opmux-backend")
    config_file = os.environ.get(
        "OPMUX_FAKE_CONFIG_FILE", f"{workdir}/docker-compose.yml"
    )
    if kind == "foreign":
        labels = {
            "com.docker.compose.project": os.environ.get(
                "OPMUX_FAKE_FOREIGN_PROJECT", "someone-else"
            ),
            "com.docker.compose.service": os.environ.get(
                "OPMUX_FAKE_FOREIGN_SERVICE", service
            ),
            "com.docker.compose.project.working_dir": os.environ.get(
                "OPMUX_FAKE_FOREIGN_WORKDIR", "/opt/other-project"
            ),
            "com.docker.compose.project.config_files": os.environ.get(
                "OPMUX_FAKE_FOREIGN_CONFIG",
                "/opt/other-project/docker-compose.yml",
            ),
        }
    else:
        labels = {
            "com.docker.compose.project": OWNED_PROJECT,
            "com.docker.compose.service": service,
            "com.docker.compose.project.working_dir": workdir,
            "com.docker.compose.project.config_files": config_file,
        }
    return {
        "running": running,
        "exit_code": 0,
        "labels": labels,
        "image": os.environ.get("CONTAINER_IMAGE", "opmux-gateway:mvp")
        if service == "gateway"
        else "opmux-simulator:local",
    }


def seed() -> dict:
    images = os.environ.get(
        "OPMUX_FAKE_IMAGES", "opmux-gateway:mvp,opmux-simulator:local"
    )
    image_set = [item for item in images.split(",") if item]
    gateway_kind = os.environ.get("OPMUX_FAKE_GATEWAY", "missing")
    simulator_kind = os.environ.get("OPMUX_FAKE_SIMULATOR", "missing")
    gateway_running = os.environ.get("OPMUX_FAKE_GATEWAY_RUNNING", "true") == "true"
    simulator_running = (
        os.environ.get("OPMUX_FAKE_SIMULATOR_RUNNING", "true") == "true"
    )
    containers: dict[str, dict] = {}
    gateway = seed_container(gateway_kind, "gateway", gateway_running)
    simulator = seed_container(simulator_kind, "simulator", simulator_running)
    if gateway is not None:
        containers[GATEWAY] = gateway
    if simulator is not None:
        containers[SIMULATOR] = simulator
    containers[DB_CONTAINER] = {
        "running": True,
        "exit_code": 0,
        "labels": {},
        "image": "postgres:17",
    }
    return {"images": image_set, "containers": containers, "networks": [NETWORK]}


def load() -> dict:
    path = state_file()
    if path.exists():
        return json.loads(path.read_text())
    data = seed()
    save(data)
    return data


def save(data: dict) -> None:
    state_file().write_text(json.dumps(data))


def fail(message: str, code: int = 1) -> int:
    sys.stderr.write(message + "\n")
    return code


def inspect_format(fmt: str, container: dict) -> str:
    if fmt == "{{.State.Running}}":
        return "true" if container.get("running") else "false"
    if fmt == "{{.State.ExitCode}}":
        return str(container.get("exit_code", 0))
    if fmt == "{{.State.OOMKilled}}":
        return "false"
    if fmt == "{{.Name}}":
        return str(container.get("name", ""))
    if fmt == "{{.Config.Image}}":
        return str(container.get("image", ""))
    if fmt == "{{.Image}}":
        return str(container.get("image_id", "sha256:fake"))
    marker = '.Config.Labels "'
    if marker in fmt:
        start = fmt.index(marker) + len(marker)
        end = fmt.index('"', start)
        key = fmt[start:end]
        return str(container.get("labels", {}).get(key, ""))
    return ""


def parse_kv(args: list[str], flag: str) -> str | None:
    for index, item in enumerate(args):
        if item == flag and index + 1 < len(args):
            return args[index + 1]
        prefix = flag + "="
        if item.startswith(prefix):
            return item[len(prefix) :]
    return None


def handle_compose(args: list[str], data: dict) -> int:
    env_file = parse_kv(args, "--env-file")
    project = parse_kv(args, "--project-name")
    capture = {
        "args": args,
        "project": project,
        "env_file": env_file,
        "container_image": os.environ.get("CONTAINER_IMAGE", ""),
        "database_url_host": "",
        "database_url_has_owned_host": False,
        "database_url_has_inherited_host": False,
        "env_file_is_repo_dotenv": False,
        "env_file_is_opmux_local": False,
        "env_file_keys": [],
    }
    if env_file:
        env_path = Path(env_file)
        capture["env_file_is_repo_dotenv"] = env_path.name == ".env"
        capture["env_file_is_opmux_local"] = env_path.name == ".env.opmux-local"
        if env_path.is_file():
            keys = []
            for raw in env_path.read_text().splitlines():
                if not raw or raw.startswith("#") or "=" not in raw:
                    continue
                key, value = raw.split("=", 1)
                keys.append(key)
                if key == "OPMUX_LOCAL_DATABASE_URL":
                    capture["database_url_has_owned_host"] = (
                        "supabase_db_opmux-mvp-20260919" in value
                    )
                    capture["database_url_has_inherited_host"] = (
                        "db.example.invalid" in value or "other-db.example" in value
                    )
                    if "@" in value:
                        capture["database_url_host"] = value.split("@", 1)[1].split(
                            "/", 1
                        )[0]
            capture["env_file_keys"] = keys
    process_url = os.environ.get("OPMUX_LOCAL_DATABASE_URL", "")
    if process_url:
        capture["database_url_has_owned_host"] = (
            capture["database_url_has_owned_host"]
            or "supabase_db_opmux-mvp-20260919" in process_url
        )
        capture["database_url_has_inherited_host"] = (
            capture["database_url_has_inherited_host"]
            or "db.example.invalid" in process_url
            or "other-db.example" in process_url
        )
        if not capture["database_url_host"] and "@" in process_url:
            capture["database_url_host"] = process_url.split("@", 1)[1].split("/", 1)[
                0
            ]
    dest = capture_path()
    if dest is not None:
        dest.write_text(json.dumps(capture))

    action = None
    for item in args:
        if item in {"up", "build", "run", "ps", "config", "down"}:
            action = item
            break
    if action == "down":
        return fail("compose down should not be used for owned stack stop")
    if action == "build":
        image = os.environ.get("CONTAINER_IMAGE", "opmux-gateway:mvp")
        if "simulator" in args and "gateway" not in args:
            if "opmux-simulator:local" not in data["images"]:
                data["images"].append("opmux-simulator:local")
        else:
            if image not in data["images"]:
                data["images"].append(image)
        save(data)
        return 0
    if action in {"up", "run", "ps", "config", None}:
        if action == "up":
            workdir = os.environ.get("OPMUX_FAKE_WORKDIR", "/opt/opmux-backend")
            config_file = os.environ.get(
                "OPMUX_FAKE_CONFIG_FILE", f"{workdir}/docker-compose.yml"
            )
            image = os.environ.get("CONTAINER_IMAGE", "opmux-gateway:mvp")
            data["containers"][GATEWAY] = {
                "running": True,
                "exit_code": 0,
                "image": image,
                "labels": {
                    "com.docker.compose.project": OWNED_PROJECT,
                    "com.docker.compose.service": "gateway",
                    "com.docker.compose.project.working_dir": workdir,
                    "com.docker.compose.project.config_files": config_file,
                },
            }
            data["containers"][SIMULATOR] = {
                "running": True,
                "exit_code": 0,
                "image": "opmux-simulator:local",
                "labels": {
                    "com.docker.compose.project": OWNED_PROJECT,
                    "com.docker.compose.service": "simulator",
                    "com.docker.compose.project.working_dir": workdir,
                    "com.docker.compose.project.config_files": config_file,
                },
            }
            save(data)
        return 0
    return fail(f"unsupported compose action {action}")


def main(argv: list[str]) -> int:
    if not argv:
        return fail("missing docker command")
    append_log(" ".join(argv))
    data = load()
    cmd = argv[0]
    args = argv[1:]
    if cmd == "compose":
        return handle_compose(args, data)
    if cmd == "port":
        container = args[0] if args else ""
        if container == DB_CONTAINER:
            print(os.environ.get("OPMUX_FAKE_DOCKER_PORT", "127.0.0.1:55432"))
            return 0
        if container == GATEWAY:
            print("127.0.0.1:38080")
            return 0
        if container == SIMULATOR:
            print("127.0.0.1:38081")
            return 0
        return fail("unknown port target")
    if cmd == "exec":
        container = args[0] if args else ""
        if container != DB_CONTAINER:
            return fail("refusing exec on unowned container")
        rest = args[1:]
        if rest[:1] == ["pg_isready"]:
            return int(os.environ.get("OPMUX_FAKE_DOCKER_READY_STATUS", "0"))
        if rest[:2] == ["printenv", "POSTGRES_PASSWORD"]:
            print(os.environ.get("OPMUX_FAKE_DOCKER_PASSWORD", "owned-pass"))
            return 0
        return fail("unsupported exec")
    if cmd == "network":
        if args[:1] == ["inspect"] and NETWORK in args:
            print("{}")
            return 0
        return fail("unknown network")
    if cmd == "image":
        if args[:1] == ["inspect"]:
            tag = args[1] if len(args) > 1 else ""
            if tag in data["images"]:
                print('{"Id":"sha256:fake"}')
                return 0
            return fail(f"No such image: {tag}")
        return fail("unsupported image command")
    if cmd == "inspect":
        fmt = parse_kv(args, "-f") or parse_kv(args, "--format")
        names = [
            item
            for item in args
            if not item.startswith("-") and item != fmt
        ]
        name = names[-1] if names else ""
        container = data["containers"].get(name)
        if container is None:
            return fail(f"No such object: {name}")
        if fmt:
            print(inspect_format(fmt, container))
        else:
            print("{}")
        return 0
    if cmd == "ps":
        quiet = "-q" in args or "-aq" in args or "--quiet" in args
        if not quiet:
            return fail("unsupported docker ps format")
        for name, container in data["containers"].items():
            if name == DB_CONTAINER:
                continue
            print(name if quiet else container)
        return 0
    if cmd == "stop":
        timeout = parse_kv(args, "--time") or parse_kv(args, "-t")
        append_log(f"STOP_TIMEOUT={timeout}")
        names = [item for item in args if not item.startswith("-") and item != timeout]
        if os.environ.get("OPMUX_FAKE_STOP_FAIL") == "1":
            return fail("stop failed")
        for name in names:
            if name == DB_CONTAINER:
                return fail("refusing to stop owned database")
            container = data["containers"].get(name)
            if container is None:
                return fail(f"No such container: {name}")
            container["running"] = False
            container["exit_code"] = int(os.environ.get("OPMUX_FAKE_EXIT_CODE", "0"))
        save(data)
        return 0
    if cmd == "rm":
        if "-f" in args or "--force" in args:
            return fail("force removal is not allowed")
        names = [item for item in args if not item.startswith("-")]
        for name in names:
            if name == DB_CONTAINER:
                return fail("refusing to remove owned database")
            container = data["containers"].get(name)
            if container is None:
                return fail(f"No such container: {name}")
            if container.get("running"):
                return fail("container is running; refusing force-less rm")
            del data["containers"][name]
        save(data)
        return 0
    return fail(f"unsupported docker command {cmd}", 127)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
