#!/usr/bin/env python3
"""Dockerfile, Compose, mount, network, and optional container smoke regressions."""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def docker_daemon_available() -> bool:
    return bool(shutil.which("docker")) and subprocess.run(
        ["docker", "info"], capture_output=True
    ).returncode == 0


class DockerDeploymentTests(unittest.TestCase):
    def test_production_image_configuration_is_restrictive(self):
        dockerfile = (REPO / "Dockerfile").read_text()
        ignore = (REPO / ".dockerignore").read_text()
        compose = (REPO / "compose.yaml").read_text()
        self.assertIn("AS builder", dockerfile)
        self.assertIn("AS runtime", dockerfile)
        self.assertIn("USER 10001:10001", dockerfile)
        self.assertIn("HEALTHCHECK", dockerfile)
        self.assertIn("SIGTERM", dockerfile)
        self.assertIn("!Cargo.lock", ignore)
        self.assertIn("!crates/", ignore)
        self.assertNotIn("data/", ignore)
        self.assertIn("read_only: true", compose)
        self.assertIn("type: bind", compose)
        self.assertIn("create_host_path: false", compose)
        self.assertIn("restart: unless-stopped", compose)
        self.assertIn("cap_drop:", compose)
        self.assertIn("external: ${CHESS_DATABASE_NETWORK_EXTERNAL:-${OPENING_EXPLORER_NETWORK_EXTERNAL:-${CODB_NETWORK_EXTERNAL:-false}}}", compose)
        self.assertIn("CHESS_DATABASE_DEEP_VALIDATE_ON_STARTUP: ${CHESS_DATABASE_DEEP_VALIDATE_ON_STARTUP:-false}", compose)
        self.assertIn("CHESS_DATABASE_ENABLE_GAME_RETRIEVAL", compose)
        self.assertIn("--enable-game-retrieval", compose)
        self.assertIn("CHESS_DATABASE_ENABLE_GAME_RETRIEVAL=true", (REPO / ".env.example").read_text())
        self.assertNotIn("ports:", compose)
        self.assertIn(".env.*", (REPO / ".gitignore").read_text())
        self.assertIn("!.env.example", (REPO / ".gitignore").read_text())
        validation_ignore = (REPO / "Dockerfile.validation.dockerignore").read_text()
        self.assertIn("data/", validation_ignore)
        self.assertIn(".git/", validation_ignore)
        self.assertIn(".env.*", validation_ignore)
        validation_dockerfile = (REPO / "Dockerfile.validation").read_text()
        self.assertIn("rustup component add rustfmt clippy", validation_dockerfile)
        self.assertIn("RUN ./scripts/validate.sh", validation_dockerfile)

    @unittest.skipUnless(docker_daemon_available(), "Docker daemon is unavailable")
    def test_compose_configs_parse(self):
        subprocess.run(
            ["docker", "compose", "-f", "compose.yaml", "config", "--quiet"],
            cwd=REPO, check=True, capture_output=True, text=True,
        )
        debug_config = subprocess.run(
            ["docker", "compose", "-f", "compose.yaml", "-f",
             "compose.debug.yaml", "config", "--format", "json"],
            cwd=REPO, check=True, capture_output=True, text=True,
        )
        debug_ports = json.loads(debug_config.stdout)["services"]["opening-explorer"]["ports"]
        self.assertEqual(len(debug_ports), 1)
        self.assertEqual(debug_ports[0]["host_ip"], "127.0.0.1")
        self.assertEqual(debug_ports[0]["published"], "8081")
        self.assertEqual(debug_ports[0]["target"], 8080)
        config = subprocess.run(
            ["docker", "compose", "-f", "compose.yaml", "config", "--format", "json"],
            cwd=REPO, check=True, capture_output=True, text=True,
        )
        service = json.loads(config.stdout)["services"]["opening-explorer"]
        self.assertFalse(service.get("ports"))
        self.assertTrue(all(volume.get("read_only") for volume in service["volumes"]))

    @unittest.skipUnless(docker_daemon_available(), "Docker daemon is unavailable")
    def test_fixture_container_http_smoke(self):
        subprocess.run(
            [sys.executable, str(REPO / "scripts" / "docker_smoke.py"),
             "--fixtures", "--disable-game-retrieval"],
            cwd=REPO, check=True, timeout=2400,
        )


if __name__ == "__main__":
    unittest.main()
