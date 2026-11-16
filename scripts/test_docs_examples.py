#!/usr/bin/env python3
"""Keep the API documentation and its examples in step with the implementation.

Run with:  python3 -m unittest scripts/test_docs_examples.py

Three layers, from cheapest to most realistic:

1. Static contract checks (always run, no network, no Rust toolchain). They
   compare docs/API_REFERENCE.md, docs/GETTING_STARTED.md and docs/USE_CASES.md
   with the sources: route table (crates/api/src/routes/mod.rs), error codes
   (crates/api/src/error.rs), response structs (crates/api/src/models.rs),
   CLI flags and environment variables (crates/cli/src/main.rs). They also
   check links, anchors, JSON and shell snippets, and example syntax.

2. Replay checks (always run, offline). Every example program runs against a
   local fake server that replays real responses recorded into fixtures/api/
   by scripts/record_api_fixtures.py.

3. Live checks (only when OPENING_EXPLORER_TEST_URL names a running server).
   They run the same examples against the real API and check the accounting
   invariants of every position response.
"""

from __future__ import annotations

import csv
import http.server
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
import urllib.parse
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DOCS = ROOT / "docs"
EXAMPLES = ROOT / "examples"
FIXTURES = ROOT / "fixtures" / "api"
API_DOCS = ["API_REFERENCE.md", "GETTING_STARTED.md", "USE_CASES.md"]
NODE = shutil.which("node")
BASH = shutil.which("bash")
CURL = shutil.which("curl")
LIVE_URL = os.environ.get("OPENING_EXPLORER_TEST_URL", "").rstrip("/")

COLLECTIONS = ("masters-otb", "elite-online")
ADMIN_TOKEN = "quick-test-token"  # token used when fixtures were recorded
FEN_AFTER_NF3 = "rnbqkbnr/pppp1ppp/8/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R b KQkq - 1 2"

# Flags that belong to tools other than the `opening-explorer` CLI and are
# used in documented shell snippets or the Python example.
EXTERNAL_FLAGS = {
    "--get", "--data-urlencode", "--fail", "--silent", "--show-error", "--request",
    "--header", "--write-out", "--locked", "--release", "--package", "--help",
    "--url", "--version",
}

# Client-side error codes raised by examples/js/opening-explorer.mjs that are
# not server error codes.
CLIENT_SIDE_CODES = {"network_error", "http_error", "invalid_position_arguments",
                     "invalid_game_id", "invalid_collection"}


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def doc_text(name: str) -> str:
    return read(DOCS / name)


def frontmatter(text: str) -> dict[str, str]:
    match = re.match(r"^---\n(.*?)\n---(?:\n|$)", text, re.DOTALL)
    if not match:
        return {}
    fields = {}
    for line in match.group(1).splitlines():
        m = re.match(r"^([a-z_]+):\s*(.+)$", line)
        if m:
            fields[m.group(1)] = m.group(2).strip().strip('"\'')
    return fields


def fenced_blocks(text: str, language: str) -> list[str]:
    """Return the bodies of fenced code blocks whose info string starts with `language`."""
    blocks, current, inside, fence = [], [], False, None
    for line in text.splitlines():
        if not inside:
            m = re.match(r"^(`{3,}|~{3,})\s*([A-Za-z0-9_+-]*)", line)
            if m and m.group(2) == language:
                inside, fence = True, m.group(1)
                current = []
        else:
            if line.strip() == fence:
                blocks.append("\n".join(current))
                inside = False
            else:
                current.append(line)
    return blocks


def github_slug(heading: str) -> str:
    heading = heading.strip().replace("`", "")
    heading = re.sub(r"[^\w\- ]", "", heading.lower())
    return heading.replace(" ", "-")


def heading_slugs(text: str) -> set[str]:
    slugs = set()
    in_fence = False
    for line in text.splitlines():
        if line.startswith("```") or line.startswith("~~~"):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        m = re.match(r"^#{1,6}\s+(.*)$", line)
        if m:
            slugs.add(github_slug(m.group(1)))
    return slugs


def route_templates() -> set[str]:
    source = read(ROOT / "crates" / "api" / "src" / "routes" / "mod.rs")
    return set(re.findall(r'\.route\(\s*"([^"]+)"', source))


def error_codes_from_source() -> set[str]:
    source = read(ROOT / "crates" / "api" / "src" / "error.rs")
    body = source.split("pub fn error_code", 1)[1].split("pub fn status_code", 1)[0]
    return set(re.findall(r'=>\s*"([a-z_]+)"', body))


def documented_error_codes() -> set[str]:
    text = doc_text("API_REFERENCE.md")
    return set(re.findall(r"^\|\s*`([a-z_]+)`\s*\|\s*\d{3}\s*\|", text, re.MULTILINE))


def struct_fields(source: str, name: str) -> list[str]:
    match = re.search(r"pub struct " + name + r" \{(.*?)\n\}", source, re.DOTALL)
    if not match:
        return []
    return re.findall(r"^\s*pub (\w+):", match.group(1), re.MULTILINE)


def cli_serve_env_names() -> set[str]:
    return set(re.findall(r'env\s*=\s*"([A-Z_]+)"', read(ROOT / "crates" / "cli" / "src" / "main.rs")))


def cli_long_flags() -> set[str]:
    """Kebab-case names of every `#[arg(...)]` field in the CLI."""
    flags, pending = set(), False
    for line in read(ROOT / "crates" / "cli" / "src" / "main.rs").splitlines():
        if "#[arg(" in line:
            pending = True
            continue
        if pending:
            # Attributes may span several lines; keep waiting for the field name.
            m = re.match(r"^\s*([a-z_][a-z0-9_]*):", line)
            if m:
                flags.add("--" + m.group(1).replace("_", "-"))
                pending = False
            elif line.strip().startswith("}"):
                pending = False
    explicit = set(re.findall(r'long\s*=\s*"([a-z0-9-]+)"', read(ROOT / "crates" / "cli" / "src" / "main.rs")))
    return flags | {"--" + name for name in explicit}


def position_invariant_errors(body: dict) -> list[str]:
    errors = []
    total = body["total_games"]
    if body["white_wins"] + body["draws"] + body["black_wins"] != total:
        errors.append("white_wins + draws + black_wins != total_games")
    if sum(m["total_games"] for m in body["moves"]) != total:
        errors.append("sum of move totals != total_games")
    keys = [(-m["total_games"], m["uci"]) for m in body["moves"]]
    if keys != sorted(keys):
        errors.append("moves not sorted by total desc, uci asc")
    for m in body["moves"]:
        if m["white_wins"] + m["draws"] + m["black_wins"] != m["total_games"]:
            errors.append(f"move {m['uci']} W+D+B != total")
    return errors


class StaticDocumentationContract(unittest.TestCase):
    """Documentation must agree with the implementation it describes."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.texts = {name: doc_text(name) for name in API_DOCS}

    def test_frontmatter_on_canonical_docs(self) -> None:
        for name in API_DOCS:
            fields = frontmatter(self.texts[name])
            for required in ("summary", "read_when"):
                self.assertTrue(fields.get(required), f"{name}: missing {required}")
            self.assertEqual(fields.get("status"), "current", f"{name}: status must be current")

    def test_relative_links_and_anchors_resolve(self) -> None:
        problems = []
        sources = dict(self.texts)
        sources["examples"] = ""
        link = re.compile(r"(?<!!)\[[^\]]*\]\(([^)\s]+)\)")
        for name in API_DOCS:
            text = re.sub(r"```.*?```", "", self.texts[name], flags=re.DOTALL)
            for target in link.findall(text):
                if re.match(r"^(https?:|mailto:)", target):
                    continue
                path_part, _, anchor = target.partition("#")
                target_file = (DOCS / path_part).resolve() if path_part else DOCS / name
                if path_part and not target_file.exists():
                    problems.append(f"{name} -> {target} (missing file)")
                    continue
                if anchor and target_file.suffix == ".md":
                    slugs = heading_slugs(read(target_file))
                    if anchor not in slugs:
                        problems.append(f"{name} -> {target} (missing anchor)")
        self.assertEqual(problems, [])

    def test_documented_routes_exist(self) -> None:
        routes = route_templates()
        self.assertTrue(routes, "route table not found")
        normalized_routes = {re.sub(r"\{[^}]+\}", "{}", r) for r in routes}
        problems = []
        for name in API_DOCS:
            for path in re.findall(r"(/(?:v1/[A-Za-z0-9_{}/\-.]*|health|ready))(?=[\s'\"`)?\]>,.]|$)", self.texts[name]):
                if path in ("/v1", "/v1/"):
                    continue
                normalized = re.sub(r"/(masters-otb|elite-online)(?=/|$)", "/{}", path)
                normalized = re.sub(r"/\d+(?=/|$)", "/{}", normalized)
                normalized = re.sub(r"\{[^}]+\}", "{}", normalized)
                if normalized not in normalized_routes:
                    problems.append(f"{name}: {path}")
        self.assertEqual(problems, [], "documented paths not in the router")

    def test_documented_query_parameters_are_supported(self) -> None:
        supported = {"collection", "moves", "fen", "since_year", "min_rating"}
        used = set()
        for name in API_DOCS:
            used |= set(re.findall(r"--data-urlencode\s+['\"]([a-z_]+)=", self.texts[name]))
            used |= set(re.findall(r"[?&]([a-z_]+)=", self.texts[name]))
        for script in (EXAMPLES / "curl").glob("*.sh"):
            used |= set(re.findall(r"--data-urlencode\s+[\"']([a-z_]+)=", read(script)))
        unknown = sorted(used - supported)
        self.assertEqual(unknown, [], "query parameters not accepted by the server")

    def test_error_code_table_matches_source(self) -> None:
        source = error_codes_from_source()
        documented = documented_error_codes()
        self.assertTrue(source, "no error codes parsed from error.rs")
        self.assertEqual(sorted(documented - source), [], "documented codes missing from error.rs")
        self.assertEqual(sorted(source - documented), [], "error.rs codes missing from the reference")

    def test_documented_response_fields_exist_in_models(self) -> None:
        models = read(ROOT / "crates" / "api" / "src" / "models.rs")
        expected = {
            "PositionResponse": None,
            "CandidateMoveResponse": None,
            "CollectionMetadataResponse": None,
            "HealthResponse": None,
            "ReadyResponse": None,
            "AdminReloadResponse": None,
        }
        reference = self.texts["API_REFERENCE.md"]
        missing = []
        for struct in expected:
            fields = struct_fields(models, struct)
            self.assertTrue(fields, f"{struct} not found in models.rs")
            for field in fields:
                if f"`{field}`" not in reference and f'"{field}"' not in reference:
                    missing.append(f"{struct}.{field}")
        self.assertEqual(missing, [], "response fields not documented")

    def test_json_examples_are_valid(self) -> None:
        bad = []
        for name in API_DOCS:
            for block in fenced_blocks(self.texts[name], "json"):
                try:
                    json.loads(block)
                except json.JSONDecodeError as error:
                    bad.append(f"{name}: {error}")
        self.assertEqual(bad, [])

    @unittest.skipUnless(BASH, "bash not available")
    def test_shell_examples_have_valid_syntax(self) -> None:
        bad = []
        for name in API_DOCS:
            for block in fenced_blocks(self.texts[name], "sh"):
                result = subprocess.run([BASH, "-n"], input=block, text=True, capture_output=True)
                if result.returncode != 0:
                    bad.append(f"{name}: {result.stderr.strip()}")
        self.assertEqual(bad, [])

    @unittest.skipUnless(NODE, "node not available")
    def test_javascript_snippets_parse(self) -> None:
        bad = []
        with tempfile.TemporaryDirectory() as tmp:
            for name in API_DOCS:
                for index, block in enumerate(fenced_blocks(self.texts[name], "js")):
                    path = Path(tmp) / f"{name}-{index}.mjs"
                    path.write_text(block, encoding="utf-8")
                    result = subprocess.run([NODE, "--check", str(path)], capture_output=True, text=True)
                    if result.returncode != 0:
                        bad.append(f"{name} block {index}: {result.stderr.strip().splitlines()[-1]}")
        self.assertEqual(bad, [])

    def test_documented_cli_flags_exist(self) -> None:
        known = cli_long_flags() | {"--help", "--version", "--json"}
        used = set()
        for name in API_DOCS:
            used |= set(re.findall(r"(?<![\w-])(--[a-z][a-z0-9-]*)", self.texts[name]))
        for script in list(EXAMPLES.rglob("*.sh")) + list(EXAMPLES.rglob("*.py")):
            used |= set(re.findall(r"(?<![\w-])(--[a-z][a-z0-9-]*)", read(script)))
        unknown = sorted(used - known - EXTERNAL_FLAGS)
        self.assertEqual(unknown, [], "flags documented but not defined by the CLI")

    def test_documented_environment_variables_exist(self) -> None:
        # Variables read by the example clients and scripts, not by the server.
        client_only = {"OPENING_EXPLORER_URL", "OPENING_EXPLORER_COLLECTION"}
        documented = set()
        for name in API_DOCS:
            documented |= set(re.findall(r"OPENING_EXPLORER_[A-Z_]*[A-Z]", self.texts[name]))
        documented -= client_only
        defined = cli_serve_env_names()
        self.assertEqual(sorted(documented - defined), [], "documented variables not read by the CLI")
        served = {name for name in defined if name.startswith("OPENING_EXPLORER_")}
        self.assertEqual(sorted(served - documented), [], "CLI variables missing from the reference")


class ExampleSyntax(unittest.TestCase):
    @unittest.skipUnless(BASH, "bash not available")
    def test_curl_scripts_parse(self) -> None:
        for script in sorted((EXAMPLES / "curl").glob("*.sh")):
            result = subprocess.run([BASH, "-n", str(script)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, f"{script.name}: {result.stderr}")

    @unittest.skipUnless(NODE, "node not available")
    def test_javascript_examples_parse(self) -> None:
        for script in sorted((EXAMPLES / "js").glob("*.mjs")):
            result = subprocess.run([NODE, "--check", str(script)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, f"{script.name}: {result.stderr}")

    def test_python_examples_compile(self) -> None:
        for script in sorted((EXAMPLES / "python").glob("*.py")):
            compile(read(script), str(script), "exec")


class _FakeExplorerHandler(http.server.BaseHTTPRequestHandler):
    """Replays recorded responses from fixtures/api/ for the example programs."""

    samples: dict[str, dict] = {}
    sample_game_id = ""
    seen: list[dict] = []

    def log_message(self, *args) -> None:  # keep test output quiet
        pass

    def do_GET(self) -> None:
        self._serve("GET")

    def do_POST(self) -> None:
        self._serve("POST")

    def _pick(self, method: str, path: str, query: dict[str, str]) -> str | None:
        if method == "GET" and path == "/health":
            return "health"
        if method == "GET" and path == "/ready":
            return "ready"
        if method == "GET" and path == "/v1/collections":
            return "collections"
        if method == "POST" and path == "/v1/admin/reload":
            if self.headers.get("Origin"):
                return "admin_unauthorized"
            if self.headers.get("Authorization") == f"Bearer {ADMIN_TOKEN}" or \
                    self.headers.get("X-Admin-Token") == ADMIN_TOKEN:
                return "admin_reload_ok"
            return "admin_unauthorized"
        if method == "GET" and path == "/v1/explorer/position":
            return self._pick_position(query)
        match = re.fullmatch(r"/v1/collections/([^/]+)/games/([^/]+)", path)
        if method == "GET" and match:
            collection, game_id = match.groups()
            if collection not in COLLECTIONS:
                return "error_invalid_collection"
            if not re.fullmatch(r"\d+", game_id) or int(game_id) > 2**64 - 1:
                return "error_game_invalid_id"
            if game_id == self.sample_game_id:
                return "game_elite" if collection == "elite-online" else "error_game_collection_mismatch"
            return "error_game_not_found"
        return "error_not_found"

    def _pick_position(self, query: dict[str, str]) -> str:
        collection = query.get("collection")
        if collection not in COLLECTIONS:
            return "error_invalid_collection"
        if "since_year" in query or "min_rating" in query:
            return "error_unsupported_parameter"
        short = "masters" if collection == "masters-otb" else "elite"
        if "moves" in query and "fen" in query:
            return "error_ambiguous_position"
        if "fen" in query:
            return f"position_{short}_fen" if query["fen"] == FEN_AFTER_NF3 else "error_invalid_fen"
        if "moves" not in query:
            return "error_missing_position"
        tokens = [t for t in re.split(r"[,\s]+", query["moves"]) if t]
        if len(tokens) > 200:
            return "error_move_sequence_too_long"
        if tokens in (["e4", "e5", "Nf3"], ["e2e4", "e7e5", "g1f3"]):
            return f"position_{short}_moves"
        return "error_invalid_moves"

    def _serve(self, method: str) -> None:
        parts = urllib.parse.urlsplit(self.path)
        query = dict(urllib.parse.parse_qsl(parts.query, keep_blank_values=True))
        type(self).seen.append({"method": method, "path": parts.path,
                                "headers": {k.lower(): v for k, v in self.headers.items()}})
        name = self._pick(method, parts.path, query)
        sample = self.samples.get(name) if name else None
        if sample is None:
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        headers = sample.get("headers", {})
        etag = headers.get("etag")
        if etag and self.headers.get("If-None-Match") in (etag, "*"):
            self.send_response(304)
            self.send_header("ETag", etag)
            self.send_header("Cache-Control", headers.get("cache-control", ""))
            self.end_headers()
            return
        if "json" in sample:
            body = json.dumps(sample["json"]).encode("utf-8")
        else:
            body = sample.get("text", "").encode("utf-8")
        self.send_response(sample["status"])
        for key, value in headers.items():
            self.send_header(key, value)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def load_samples() -> tuple[dict[str, dict], str]:
    samples = {}
    for path in sorted(FIXTURES.glob("*.json")):
        if path.name == "manifest.json":
            continue
        samples[path.stem] = json.loads(read(path))
    manifest = json.loads(read(FIXTURES / "manifest.json"))
    return samples, manifest.get("sample_game_id") or ""


class ReplayExamples(unittest.TestCase):
    """Run every example against replayed responses from a local fake server."""

    @classmethod
    def setUpClass(cls) -> None:
        samples, game_id = load_samples()
        _FakeExplorerHandler.samples = samples
        _FakeExplorerHandler.sample_game_id = game_id
        cls.game_id = game_id
        cls.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _FakeExplorerHandler)
        cls.base = f"http://127.0.0.1:{cls.server.server_address[1]}"
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()

    @classmethod
    def tearDownClass(cls) -> None:
        cls.server.shutdown()
        cls.server.server_close()

    def setUp(self) -> None:
        _FakeExplorerHandler.seen = []
        self.env = {**os.environ, "OPENING_EXPLORER_URL": self.base}

    def _run(self, argv: list[str], env: dict[str, str] | None = None) -> subprocess.CompletedProcess:
        return subprocess.run(argv, cwd=ROOT, env=env or self.env, capture_output=True, text=True,
                              timeout=120)

    @unittest.skipUnless(NODE and BASH and CURL, "node, bash and curl are required")
    def test_curl_quickstart(self) -> None:
        result = self._run([BASH, str(EXAMPLES / "curl" / "quickstart.sh")],
                           env={**self.env, "JSON_FORMATTER": "cat"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("masters-otb", result.stdout)
        self.assertIn('"total_games"', result.stdout)

    @unittest.skipUnless(BASH and CURL, "bash and curl are required")
    def test_curl_position_script(self) -> None:
        result = self._run([BASH, str(EXAMPLES / "curl" / "position.sh"),
                            "elite-online", "--moves", "e4,e5,Nf3"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("HTTP 200", result.stdout)
        error = self._run([BASH, str(EXAMPLES / "curl" / "position.sh"),
                           "masters-otb", "--moves", "e4,e4"])
        self.assertEqual(error.returncode, 0)
        self.assertIn("invalid_moves", error.stdout)
        self.assertIn("HTTP 400", error.stdout)

    @unittest.skipUnless(BASH and CURL, "bash and curl are required")
    def test_curl_game_script(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "game.pgn"
            result = self._run([BASH, str(EXAMPLES / "curl" / "game.sh"),
                                "elite-online", self.game_id, str(out)])
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue(out.read_text(encoding="utf-8").startswith("[Event "))
            refused = self._run([BASH, str(EXAMPLES / "curl" / "game.sh"), "masters-otb", self.game_id])
            self.assertNotEqual(refused.returncode, 0)
            invalid = self._run([BASH, str(EXAMPLES / "curl" / "game.sh"), "elite-online", "12x"])
            self.assertEqual(invalid.returncode, 2)

    @unittest.skipUnless(BASH and CURL, "bash and curl are required")
    def test_curl_reload_script(self) -> None:
        ok = self._run([BASH, str(EXAMPLES / "curl" / "reload.sh")],
                       env={**self.env, "OPENING_EXPLORER_ADMIN_TOKEN": ADMIN_TOKEN})
        self.assertEqual(ok.returncode, 0, ok.stderr)
        self.assertIn("reloaded_collections", ok.stdout)
        self.assertTrue(any(s["path"] == "/v1/admin/reload" and
                            s["headers"].get("authorization") == f"Bearer {ADMIN_TOKEN}"
                            for s in _FakeExplorerHandler.seen))
        missing = self._run([BASH, str(EXAMPLES / "curl" / "reload.sh")],
                            env={k: v for k, v in self.env.items() if k != "OPENING_EXPLORER_ADMIN_TOKEN"})
        self.assertEqual(missing.returncode, 2)

    @unittest.skipUnless(NODE, "node is required")
    def test_javascript_quickstart(self) -> None:
        result = self._run([NODE, str(EXAMPLES / "js" / "quickstart.mjs")])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Loaded collections:", result.stdout)
        self.assertIn("FEN lookup of the same position matches: true", result.stdout)
        self.assertTrue(any("if-none-match" in s["headers"] for s in _FakeExplorerHandler.seen),
                        "second position request should be conditional")

    @unittest.skipUnless(NODE, "node is required")
    def test_javascript_compare_collections(self) -> None:
        result = self._run([NODE, str(EXAMPLES / "js" / "compare-collections.mjs")])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("masters-otb", result.stdout)
        self.assertIn("elite-online", result.stdout)

    @unittest.skipUnless(NODE, "node is required")
    def test_javascript_client_errors_and_ids(self) -> None:
        script = (
            "const m = await import(process.env.CLIENT_URL);\n"
            "const c = new m.OpeningExplorerClient({ baseUrl: process.env.OPENING_EXPLORER_URL });\n"
            "const out = {};\n"
            "const tryCode = async (label, fn) => { try { await fn(); out[label] = 'ok'; }"
            " catch (e) { out[label] = [e.status, e.code]; } };\n"
            "await tryCode('illegal', () => c.position({ collection: 'masters-otb', moves: 'e4,e4' }));\n"
            "await tryCode('both', () => c.position({ collection: 'masters-otb', moves: 'e4', fen: 'x' }));\n"
            "await tryCode('badCollection', () => c.position({ collection: 'nope', moves: 'e4' }));\n"
            "await tryCode('tooBig', () => c.game('elite-online', '18446744073709551616'));\n"
            "await tryCode('notDigits', () => c.game('elite-online', '1.5'));\n"
            "await tryCode('unknownGame', () => c.game('elite-online', '1'));\n"
            "const pgn = await c.game('elite-online', process.env.GAME_ID);\n"
            "out.pgnStart = pgn.slice(0, 7);\n"
            "const a = await c.position({ collection: 'masters-otb', moves: 'e4,e5,Nf3' });\n"
            "const b = await c.position({ collection: 'masters-otb', moves: 'e4,e5,Nf3' });\n"
            "out.sameBodyFromCache = a === b;\n"
            "console.log(JSON.stringify(out));\n"
        )
        result = self._run([NODE, "--input-type=module", "-e", script],
                           env={**self.env, "CLIENT_URL": (EXAMPLES / "js" / "opening-explorer.mjs").as_uri(),
                                "GAME_ID": self.game_id})
        self.assertEqual(result.returncode, 0, result.stderr)
        out = json.loads(result.stdout.strip().splitlines()[-1])
        self.assertEqual(out["illegal"], [400, "invalid_moves"])
        self.assertEqual(out["both"][1], "invalid_position_arguments")
        self.assertEqual(out["badCollection"][1], "invalid_collection")
        self.assertEqual(out["tooBig"][1], "invalid_game_id")
        self.assertEqual(out["notDigits"][1], "invalid_game_id")
        self.assertEqual(out["unknownGame"], [404, "game_not_found"])
        self.assertEqual(out["pgnStart"], "[Event ")
        self.assertTrue(out["sameBodyFromCache"], "304 should reuse the cached body")

    def test_python_position_command(self) -> None:
        result = self._run([sys.executable, "-I", str(EXAMPLES / "python" / "opening_explorer.py"),
                            "--url", self.base, "position", "--collection", "masters-otb",
                            "--moves", "e4,e5,Nf3"])
        self.assertEqual(result.returncode, 0, result.stderr)
        body = json.loads(result.stdout)
        self.assertEqual(position_invariant_errors(body), [])
        self.assertEqual(body["collection"], "masters-otb")
        self.assertIsInstance(body["cpi_depth_plies"], int)
        self.assertGreater(body["cpi_depth_plies"], 0)

    def test_python_batch_command_deduplicates_positions(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            inputs = Path(tmp) / "openings.txt"
            outputs = Path(tmp) / "stats.csv"
            inputs.write_text("# comment\n\ne4,e5,Nf3\ne2e4 e7e5 g1f3\n", encoding="utf-8")
            result = self._run([sys.executable, "-I", str(EXAMPLES / "python" / "opening_explorer.py"),
                                "--url", self.base, "batch", "--collection", "elite-online",
                                "--input", str(inputs), "--output", str(outputs)])
            self.assertEqual(result.returncode, 0, result.stderr)
            with outputs.open(newline="", encoding="utf-8") as handle:
                rows = list(csv.DictReader(handle))
            self.assertEqual(len(rows), 1, "two move orders reaching one board give one row")
            self.assertEqual(rows[0]["collection"], "elite-online")
            self.assertEqual(rows[0]["top_move_san"], "Nc6")

    def test_python_client_reports_api_errors(self) -> None:
        result = self._run([sys.executable, "-I", str(EXAMPLES / "python" / "opening_explorer.py"),
                            "--url", self.base, "position", "--collection", "masters-otb", "--moves", "e4,e4"])
        self.assertEqual(result.returncode, 1)
        self.assertIn("invalid_moves", result.stderr)


class LiveExamples(unittest.TestCase):
    """Run the examples against a real server when OPENING_EXPLORER_TEST_URL is set."""

    @unittest.skipUnless(LIVE_URL and BASH and CURL and NODE,
                         "set OPENING_EXPLORER_TEST_URL to run live checks")
    def test_live_quickstart_and_invariants(self) -> None:
        env = {**os.environ, "OPENING_EXPLORER_URL": LIVE_URL, "JSON_FORMATTER": "cat"}
        curl_run = subprocess.run([BASH, str(EXAMPLES / "curl" / "quickstart.sh")], env=env,
                                  capture_output=True, text=True, timeout=120)
        self.assertEqual(curl_run.returncode, 0, curl_run.stderr)
        js_run = subprocess.run([NODE, str(EXAMPLES / "js" / "quickstart.mjs")], env=env,
                                capture_output=True, text=True, timeout=120)
        self.assertEqual(js_run.returncode, 0, js_run.stderr)
        py_run = subprocess.run([sys.executable, "-I", str(EXAMPLES / "python" / "opening_explorer.py"),
                                 "--url", LIVE_URL, "position", "--collection", "elite-online",
                                 "--moves", "e4,e5,Nf3"], capture_output=True, text=True, timeout=120)
        self.assertEqual(py_run.returncode, 0, py_run.stderr)
        self.assertEqual(position_invariant_errors(json.loads(py_run.stdout)), [])


if __name__ == "__main__":
    unittest.main()
