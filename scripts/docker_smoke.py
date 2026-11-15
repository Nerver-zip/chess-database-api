#!/usr/bin/env python3
"""Container integration check for CODB, with deterministic fixtures or real releases.

Fixture mode is suitable for CI. Pass --real --data-dir PATH to exercise provisioned
production releases without copying or modifying them.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import traceback
import uuid
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from urllib.parse import quote

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "scripts"))
import corpus  # noqa: E402

IMAGE = "codb-opening-explorer:local"
CLIENT_IMAGE = "curlimages/curl@sha256:94e9e444bcba979c2ea12e27ae39bee4cd10bc7041a472c4727a558e213744e6"


def run(args: list[str], *, check: bool = True, capture: bool = True,
        timeout: int = 300) -> subprocess.CompletedProcess:
    result = subprocess.run(args, cwd=REPO, text=True, capture_output=capture, timeout=timeout)
    if check and result.returncode:
        raise RuntimeError(f"command failed ({result.returncode}): {' '.join(args)}\n"
                           f"{result.stdout or ''}{result.stderr or ''}")
    return result


def docker_available() -> bool:
    if not shutil.which("docker"):
        return False
    return subprocess.run(["docker", "info"], capture_output=True).returncode == 0


def build_fixture_datasets(codb: Path, root: Path) -> dict[str, Path]:
    root.mkdir(parents=True, exist_ok=True)
    output: dict[str, Path] = {}
    fixtures = {
        "masters-otb": REPO / "fixtures" / "corpus" / "broadcast_sample.pgn",
        "elite-online": REPO / "fixtures" / "corpus" / "standard_sample.pgn",
    }
    for collection, pgn in fixtures.items():
        dataset = root / collection
        depth = "40" if collection == "masters-otb" else "30"
        run([str(codb), "build", "--input", str(pgn), "--collection", collection,
             "--output", str(dataset)], timeout=600)
        run([str(codb), "build-index", "--dataset", str(dataset), "--collection", collection,
             "--depth-plies", depth, "--memory-limit-mib", "16"], timeout=600)
        run([str(codb), "verify", "--dataset", str(dataset), "--deep"], timeout=600)
        output[collection] = dataset
    return output


def populate_volume(name: str, source: Path) -> None:
    run(["docker", "volume", "create", name])
    loader = run([
        "docker", "create", "--volume", f"{name}:/dataset",
        "--entrypoint", "/bin/sh", IMAGE, "-c", "sleep 3600",
    ]).stdout.strip()
    try:
        run(["docker", "cp", f"{source}/.", f"{loader}:/dataset/"], timeout=300)
    finally:
        subprocess.run(["docker", "rm", "-f", loader], capture_output=True)


def write_compose_env(path: Path, roots: dict[str, Path], network: str) -> None:
    path.write_text(
        f"CODB_DATA_DIR={path.parent}\n"
        f"CODB_MASTERS_RELEASE_DIR={roots['masters-otb']}\n"
        f"CODB_ELITE_RELEASE_DIR={roots['elite-online']}\n"
        f"CODB_NETWORK_NAME={network}\n"
        "CODB_NETWORK_EXTERNAL=false\n"
        "CODB_MEMORY_LIMIT=512m\n"
        "CODB_CPU_LIMIT=2\n"
        "CODB_READER_POOL_SIZE=8\n"
        "CODB_READER_POOL_TIMEOUT_SECS=5\n"
        "CODB_REQUEST_TIMEOUT_SECS=15\n"
        "CODB_MAX_MOVE_PLIES=200\n"
        "CODB_LOG_LEVEL=info\n"
    )


def compose_volume_override(path: Path, masters_volume: str, elite_volume: str) -> None:
    path.write_text(
        "services:\n"
        "  codb:\n"
        "    volumes:\n"
        "      - type: volume\n"
        "        source: masters_release\n"
        "        target: /datasets/masters-otb\n"
        "        read_only: true\n"
        "      - type: volume\n"
        "        source: elite_release\n"
        "        target: /datasets/elite-online\n"
        "        read_only: true\n"
        "volumes:\n"
        "  masters_release:\n"
        "    external: true\n"
        f"    name: {masters_volume}\n"
        "  elite_release:\n"
        "    external: true\n"
        f"    name: {elite_volume}\n"
    )


def host_bind_mounts_available(roots: dict[str, Path]) -> bool:
    for source in roots.values():
        result = subprocess.run(
            [
                "docker", "run", "--rm",
                "--mount", f"type=bind,source={source},target=/dataset,readonly",
                "--entrypoint", "/bin/sh", CLIENT_IMAGE, "-c",
                "test -f /dataset/current/manifest.json",
            ],
            capture_output=True, text=True,
        )
        if result.returncode != 0:
            return False
    return True


def curl_from_client(client: str, path: str, *, method: str = "GET") -> tuple[int, bytes]:
    url = f"http://codb:8080{path}"
    result = run([
        "docker", "exec", client, "/usr/bin/curl", "--silent", "--show-error",
        "--connect-timeout", "3", "--max-time", "20", "--request", method,
        "--output", "-", "--write-out", "\n%{http_code}", url,
    ])
    body, status = result.stdout.rsplit("\n", 1)
    return int(status), body.encode()


def wait_healthy(project: str, container: str, timeout_seconds: float = 240) -> None:
    deadline = time.monotonic() + timeout_seconds
    while time.monotonic() < deadline:
        inspect = subprocess.run(
            ["docker", "inspect", "--format", "{{.State.Health.Status}}", container],
            text=True, capture_output=True,
        )
        if inspect.returncode == 0 and inspect.stdout.strip() == "healthy":
            return
        state = subprocess.run(
            ["docker", "inspect", "--format", "{{.State.Status}}", container],
            text=True, capture_output=True,
        )
        if state.returncode == 0 and state.stdout.strip() == "exited":
            logs = subprocess.run(["docker", "logs", container], text=True, capture_output=True)
            raise RuntimeError(f"CODB container exited before readiness:\n{logs.stdout}{logs.stderr}")
        time.sleep(2)
    logs = subprocess.run(["docker", "logs", container], text=True, capture_output=True)
    state = subprocess.run(
        ["docker", "inspect", "--format", "{{json .State}}", container],
        text=True, capture_output=True,
    )
    readiness = subprocess.run(
        ["docker", "exec", container, "/usr/bin/curl", "--silent", "--show-error",
         "--max-time", "5", "http://127.0.0.1:8080/ready"],
        text=True, capture_output=True,
    )
    raise RuntimeError(
        f"CODB did not become healthy within {timeout_seconds:g} seconds:\n"
        f"state={state.stdout}{state.stderr}\n"
        f"direct readiness={readiness.returncode}: {readiness.stdout}{readiness.stderr}\n"
        f"logs={logs.stdout}{logs.stderr}"
    )


def check_live_api(codb: Path, roots: dict[str, Path], client: str,
                   expected_counts: dict[str, int] | None = None) -> dict[str, dict]:
    status, body = curl_from_client(client, "/health")
    assert status == 200, (status, body)
    status, ready = curl_from_client(client, "/ready")
    assert status == 200, (status, ready)
    ready_json = json.loads(ready)
    assert set(ready_json["collections_loaded"]) == set(corpus.COLLECTIONS), ready_json

    status, collections_body = curl_from_client(client, "/v1/collections")
    assert status == 200, (status, collections_body)
    metadata = json.loads(collections_body)["collections"]
    by_collection = {entry["collection"]: entry for entry in metadata}
    assert set(by_collection) == set(corpus.COLLECTIONS), by_collection

    queries: dict[str, dict] = {}
    for collection in corpus.COLLECTIONS:
        dataset = roots[collection]
        assert by_collection[collection]["total_games"] > 0
        position_args = ["query", "--dataset", str(dataset), "--moves", "e4,e5,Nf3", "--json"]
        cli = corpus.codb_json(codb, position_args)
        path = (f"/v1/explorer/position?collection={collection}"
                "&moves=e4%2Ce5%2CNf3")
        status, body = curl_from_client(client, path)
        assert status == 200, (status, body)
        response = json.loads(body)
        assert response["collection"] == collection, response
        for field in ("total_games", "white_wins", "draws", "black_wins"):
            assert response[field] == cli[field], (collection, field, response, cli)
        assert corpus.normalize_moves(response["moves"]) == corpus.normalize_moves(cli["moves"]), (
            collection, response["moves"], cli["moves"]
        )
        if expected_counts and collection in expected_counts:
            assert response["total_games"] == expected_counts[collection], (
                collection, response["total_games"], expected_counts[collection]
            )

        version = corpus.current_version_dir(dataset)
        ids = corpus.read_game_ids(version)
        assert ids, f"fixture or release for {collection} contains no games"
        status, pgn = curl_from_client(
            client, f"/v1/collections/{collection}/games/{ids[0]}"
        )
        assert status == 200, (status, pgn)
        assert pgn.lstrip().startswith(b"[Event "), pgn[:200]
        queries[collection] = {
            "dataset_version": response["dataset_version"],
            "position_http_response": response,
            "dataset_total_games": by_collection[collection]["total_games"],
            "cli_match": True,
            "expected_total_games_at_e4_e5_nf3": (
                expected_counts[collection] if expected_counts and collection in expected_counts else None
            ),
            "total_games_at_e4_e5_nf3": response["total_games"],
            "white_wins": response["white_wins"],
            "draws": response["draws"],
            "black_wins": response["black_wins"],
            "top_moves": response["moves"][:3],
            "sample_game_id": ids[0],
            "stored_game_http_status": status,
            "stored_game_bytes": len(pgn),
            "stored_game_sha256": hashlib.sha256(pgn).hexdigest(),
            "stored_game_is_pgn": pgn.lstrip().startswith(b"[Event "),
        }


    status, invalid = curl_from_client(
        client, "/v1/explorer/position?collection=masters-otb&moves=e2e5"
    )
    assert status == 400, (status, invalid)
    status, unknown_game = curl_from_client(
        client, "/v1/collections/masters-otb/games/18446744073709551615"
    )
    assert status == 404, (status, unknown_game)
    status, admin = curl_from_client(client, "/v1/admin/reload", method="POST")
    assert status == 403, (status, admin)
    queries["_checks"] = {
        "health_status": 200,
        "ready_status": 200,
        "collections_status": 200,
        "invalid_position_status": 400,
        "unknown_game_status": 404,
        "admin_reload_status": status,
    }
    return queries


def check_bad_starts(volumes: dict[str, str], roots: dict[str, Path], bad_root: Path) -> None:
    image_args = [
        "docker", "run", "--rm", "--read-only", "--cap-drop", "ALL",
        "--security-opt", "no-new-privileges:true",
        "--tmpfs", "/tmp:rw,noexec,nosuid,nodev,size=16m,mode=1777",
    ]
    base_command = [
        "--entrypoint", "/usr/local/bin/codb", IMAGE, "serve",
        "--host", "0.0.0.0", "--port", "8080", "--deep-validate-on-startup",
        "--masters-dataset", "/datasets/masters-otb", "--elite-dataset", "/datasets/elite-online",
    ]

    def mount(volume: str, target: str) -> list[str]:
        return ["--mount", f"type=volume,source={volume},target={target},readonly"]

    def failed_start(arguments: list[str], expected: str) -> None:
        result = run([*image_args, *arguments, *base_command], check=False, timeout=60)
        output = (result.stdout or "") + (result.stderr or "")
        assert result.returncode != 0, "invalid dataset unexpectedly started"
        assert expected.lower() in output.lower(), output[-1000:]

    # An absent Masters mount is an immediate clear startup failure.
    failed_start(mount(volumes["elite-online"], "/datasets/elite-online"), "dataset")

    # A complete dataset mounted at the wrong collection path fails identity validation.
    failed_start(
        mount(volumes["elite-online"], "/datasets/masters-otb")
        + mount(volumes["elite-online"], "/datasets/elite-online"),
        "collection",
    )

    incomplete = bad_root / "incomplete"
    shutil.copytree(roots["masters-otb"], incomplete, symlinks=True)
    incomplete_version = corpus.current_version_dir(incomplete)
    (incomplete_version / "games.idx").unlink()
    populate_volume(volumes["incomplete"], incomplete)
    failed_start(
        mount(volumes["incomplete"], "/datasets/masters-otb")
        + mount(volumes["elite-online"], "/datasets/elite-online"),
        "games.idx",
    )

    corrupt = bad_root / "corrupt"
    shutil.copytree(roots["masters-otb"], corrupt, symlinks=True)
    current = corpus.current_version_dir(corrupt)
    cpi = current / "positions.cpi"
    if not cpi.is_file():
        raise RuntimeError(f"fixture CPI is missing: {cpi}")
    content = bytearray(cpi.read_bytes())
    content[0] ^= 0xFF
    cpi.write_bytes(content)
    corrupt_volume = volumes["corrupt"]
    populate_volume(corrupt_volume, corrupt)
    failed_start(
        mount(corrupt_volume, "/datasets/masters-otb")
        + mount(volumes["elite-online"], "/datasets/elite-online"),
        "cpi",
    )


def inspect_value(container: str, template: str) -> str:
    return run(["docker", "inspect", "--format", template, container]).stdout.strip()


def runtime_rss_kib(container: str) -> int:
    shell = (
        "for f in /proc/[0-9]*/status; do "
        "name=; rss=; "
        "while read -r key value rest; do "
        "case \"$key\" in Name:) name=$value ;; VmRSS:) rss=$value ;; esac; "
        "done < \"$f\"; "
        "if [ \"$name\" = codb ] && [ -n \"$rss\" ]; then "
        "printf '%s\\n' \"$rss\"; exit 0; fi; "
        "done; exit 1"
    )
    result = run(["docker", "exec", container, "/bin/sh", "-c", shell])
    try:
        return int(result.stdout.strip())
    except ValueError as error:
        raise RuntimeError(f"could not read CODB VmRSS from container: {result.stdout}") from error


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--fixtures", action="store_true",
                      help="build and test both small deterministic fixture datasets")
    mode.add_argument("--real", action="store_true",
                      help="exercise existing releases in the configured data directory")
    parser.add_argument("--data-dir", type=Path, help="real release root (default ./data)")
    parser.add_argument("--skip-build", action="store_true", help="use the current local image")
    parser.add_argument("--report-json", type=Path, help="write machine-readable smoke evidence")
    args = parser.parse_args()

    if not docker_available():
        print("Docker daemon is unavailable; container integration check skipped.")
        return 77

    codb = corpus.find_codb(None)
    nonce = uuid.uuid4().hex[:10]
    project = f"codb-smoke-{nonce}"
    network = f"codb-smoke-net-{nonce}"
    container = f"{project}-codb-1"
    client = f"{project}-client"
    if args.fixtures:
        (REPO / "data").mkdir(exist_ok=True)
    temp = tempfile.TemporaryDirectory(prefix=f"codb-smoke-{nonce}-",
                                       dir=REPO / "data" if args.fixtures else None)
    temp_root = Path(temp.name)
    if args.fixtures:
        temp_root.mkdir(parents=True, exist_ok=True)
        roots = build_fixture_datasets(codb, temp_root / "datasets")
    else:
        data_dir = (args.data_dir or REPO / "data").resolve()
        roots = {c: data_dir / "releases" / c for c in corpus.COLLECTIONS}
        for collection, root in roots.items():
            if not root.is_dir():
                raise RuntimeError(f"real release directory is missing: {root}")
            report = subprocess.run([str(codb), "verify", "--dataset", str(root), "--deep"],
                                     capture_output=True, text=True)
            if report.returncode:
                raise RuntimeError(f"real release failed deep verification ({collection}): "
                                   f"{report.stdout}{report.stderr}")

    env_file = temp_root / "compose.env"
    write_compose_env(env_file, roots, network)
    masters_volume = f"{project}-masters"
    elite_volume = f"{project}-elite"
    corrupt_volume = f"{project}-corrupt"
    incomplete_volume = f"{project}-incomplete"
    base_compose = ["docker", "compose", "--project-name", project, "--env-file", str(env_file),
                    "-f", str(REPO / "compose.codb.yaml")]
    client_started = False
    compose_started = False
    try:
        if not args.skip_build:
            run([*base_compose, "build", "codb"], timeout=1800)
        run(["docker", "pull", CLIENT_IMAGE], timeout=600)
        if host_bind_mounts_available(roots):
            compose = base_compose
            mount_mode = "read_only_host_bind"
            mount_type = "bind"
        else:
            override = temp_root / "compose.volumes.yaml"
            compose_volume_override(override, masters_volume, elite_volume)
            compose = [*base_compose, "-f", str(override)]
            mount_mode = "read_only_named_volume_copy"
            mount_type = "volume"
        populate_volume(masters_volume, roots["masters-otb"])
        populate_volume(elite_volume, roots["elite-online"])
        run([*compose, "up", "-d"], timeout=1800)
        compose_started = True
        wait_healthy(project, container)

        run(["docker", "run", "-d", "--name", client, "--network", network,
             "--entrypoint", "/bin/sh", CLIENT_IMAGE, "-c", "while true; do sleep 3600; done"])
        client_started = True

        expected = None
        if args.real:
            expected = {"masters-otb": 3902, "elite-online": 30390}
        first = check_live_api(codb, roots, client, expected)
        check_bad_starts({"masters-otb": masters_volume, "elite-online": elite_volume,
                          "corrupt": corrupt_volume, "incomplete": incomplete_volume},
                         roots, temp_root / "invalid-datasets")
        rss_kib = runtime_rss_kib(container)
        peak_rss = [rss_kib]

        # Exercise bounded reader-pool and HTTP concurrency from the independent client.
        paths = [
            f"/v1/explorer/position?collection={c}&moves=e4%2Ce5%2CNf3"
            for c in corpus.COLLECTIONS
        ]
        def concurrent_request(index: int) -> int:
            collection = corpus.COLLECTIONS[(index // 2) % len(corpus.COLLECTIONS)]
            if index % 2 == 0:
                path = paths[corpus.COLLECTIONS.index(collection)]
                status, body = curl_from_client(client, path)
                if status == 200:
                    payload = json.loads(body)
                    assert payload["collection"] == collection and payload["total_games"] > 0
            else:
                game_id = first[collection]["sample_game_id"]
                status, body = curl_from_client(
                    client, f"/v1/collections/{collection}/games/{game_id}"
                )
                if status == 200:
                    assert body.lstrip().startswith(b"[Event "), body[:200]
            return status
        peak_rss = [runtime_rss_kib(container)]
        stop_monitor = threading.Event()

        def monitor_rss() -> None:
            while not stop_monitor.wait(0.2):
                try:
                    peak_rss.append(runtime_rss_kib(container))
                except (RuntimeError, subprocess.SubprocessError):
                    continue

        monitor = threading.Thread(target=monitor_rss, daemon=True)
        monitor.start()
        try:
            with ThreadPoolExecutor(max_workers=12) as pool:
                statuses = list(pool.map(concurrent_request, range(96)))
        finally:
            stop_monitor.set()
            monitor.join(timeout=5)
        assert all(status == 200 for status in statuses), statuses
        rss_kib = runtime_rss_kib(container)
        peak_rss.append(rss_kib)
        rootfs_read_only = inspect_value(container, "{{.HostConfig.ReadonlyRootfs}}") == "true"
        configured_user = inspect_value(container, "{{.Config.User}}")
        container_image_id = inspect_value(container, "{{.Image}}")
        image_size_bytes = int(run([
            "docker", "image", "inspect", "--format", "{{.Size}}", IMAGE
        ]).stdout)
        assert rootfs_read_only, "container root filesystem is writable"
        assert configured_user == "10001:10001", configured_user

        # Read-only mount enforcement applies even to the service's non-root UID.
        inspect = run(["docker", "inspect", "--format",
                       "{{range .Mounts}}{{.Destination}}={{.Type}}:{{.RW}} {{end}}", container])
        assert f"/datasets/masters-otb={mount_type}:false" in inspect.stdout, inspect.stdout
        assert f"/datasets/elite-online={mount_type}:false" in inspect.stdout, inspect.stdout
        write_attempt = subprocess.run(
            ["docker", "exec", container, "touch", "/datasets/masters-otb/.write-test"],
            text=True, capture_output=True,
        )
        assert write_attempt.returncode != 0, "service container wrote to its read-only corpus mount"

        inspect_ports = run(["docker", "inspect", "--format", "{{json .HostConfig.PortBindings}}",
                            container])
        assert inspect_ports.stdout.strip() in ("null", "{}"), inspect_ports.stdout

        run([*compose, "restart", "codb"], timeout=180)
        wait_healthy(project, container)
        second = check_live_api(codb, roots, client, expected)
        assert first == second, (first, second)

        # Deep-verify the mounted read-only snapshots from inside the restarted service container.
        for collection in corpus.COLLECTIONS:
            report = subprocess.run(
                ["docker", "exec", container, "/usr/local/bin/codb", "verify",
                 "--dataset", f"/datasets/{collection}", "--deep"],
                capture_output=True, text=True,
            )
            assert report.returncode == 0, (collection, report.stdout, report.stderr)

        run([*compose, "stop", "-t", "30"], timeout=60)
        logs = run([*compose, "logs", "codb"]).stdout
        graceful_shutdown = "shutdown completed gracefully" in logs.lower()
        assert graceful_shutdown, logs
        run(["docker", "rm", "-f", client], timeout=60)
        client_started = False
        run([*compose, "down", "--remove-orphans"], timeout=60)
        compose_started = False

        report = {
            "schema_version": 1,
            "mode": "real" if args.real else "fixtures",
            "service": "codb",
            "api_address": "http://codb:8080",
            "network": network,
            "independent_client_image": CLIENT_IMAGE,
            "collections": {c: first[c] for c in corpus.COLLECTIONS},
            "http_checks": first["_checks"],
            "concurrent_requests": {
                "count": len(statuses),
                "successful": sum(status == 200 for status in statuses),
                "concurrent_workers": 12,
            },
            "runtime_rss": {
                "sampled_peak_kib": max(peak_rss),
                "sampled_peak_bytes": max(peak_rss) * 1024,
                "sampling_interval_milliseconds": 200,
            },
            "container_settings": {
                "root_filesystem_read_only": rootfs_read_only,
                "configured_user": configured_user,
                "image_id": container_image_id,
                "image_size_bytes": image_size_bytes,
                "dataset_mount_mode": mount_mode,
                "masters_mount_read_only": f"/datasets/masters-otb={mount_type}:false" in inspect.stdout,
                "elite_mount_read_only": f"/datasets/elite-online={mount_type}:false" in inspect.stdout,
                "no_host_ports_published": inspect_ports.stdout.strip() in ("null", "{}"),
            },
            "bad_dataset_starts_rejected": [
                "missing collection mount", "wrong collection path", "incomplete dataset",
                "corrupted CPI",
            ],
            "container_restart": {
                "healthy_after_restart": True,
                "api_results_match_before_restart": first == second,
                "deep_dataset_verification_after_restart": True,
            },
            "graceful_sigterm_shutdown": graceful_shutdown,
            "completed_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        }
        if args.report_json:
            report_path = args.report_json if args.report_json.is_absolute() else REPO / args.report_json
            report_path.parent.mkdir(parents=True, exist_ok=True)
            report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + chr(10))
        print(json.dumps(report, indent=2, sort_keys=True))
        print("Docker fixture/real integration checks passed.")
        return 0
    finally:
        if client_started:
            subprocess.run(["docker", "rm", "-f", client], capture_output=True)
        if compose_started:
            subprocess.run([*compose, "down", "--remove-orphans"], capture_output=True)
        for volume in (masters_volume, elite_volume, corrupt_volume, incomplete_volume):
            subprocess.run(["docker", "volume", "rm", "-f", volume], capture_output=True)
        temp.cleanup()


if __name__ == "__main__":
    try:
        sys.exit(main())
    except AssertionError as error:
        traceback.print_exc()
        print(f"docker smoke assertion failed: {error}", file=sys.stderr)
        sys.exit(1)
    except Exception as error:
        print(f"docker smoke failed: {error}", file=sys.stderr)
        sys.exit(1)
