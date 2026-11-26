#!/usr/bin/env python3
"""
scripts/validate_docs.py — Documentation integrity, frontmatter, and link validator.

Verifies:
1. Verifies that all canonical documentation files exist.
2. Verifies canonical YAML frontmatter (summary, read_when, status: current).
3. Verifies that root markdown files do not have frontmatter.
4. Verifies uppercase basenames for canonical documentation.
5. Verifies that all canonical documents are indexed in docs/README.md.
6. Verifies that all ADRs are indexed in docs/adr/README.md and docs/README.md.
7. Verifies that all internal relative Markdown links and images resolve to existing files.
"""

import os
import re
import sys

ROOT_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

CANONICAL_DOCUMENTS = [
    "docs/README.md",
    "docs/adr/README.md",
    "docs/ARCHITECTURE.md",
    "docs/DATA_CONTRACTS.md",
    "docs/CORPUS_AND_DEDUPLICATION.md",
    "docs/CORPUS_BUILD.md",
    "docs/LICENSING.md",
    "docs/QA_STRATEGY.md",
    "docs/BENCHMARKS.md",
    "docs/ROADMAP.md",
    "docs/DECISIONS_AND_OPEN_QUESTIONS.md",
    "docs/GETTING_STARTED.md",
    "docs/API_REFERENCE.md",
    "docs/USE_CASES.md",
    "docs/DATASET_RELEASES.md",
    "docs/DOCKER_DEPLOYMENT.md",
    "docs/MIGRATION_FROM_CODB.md",
    "docs/NAMING_MIGRATION.md",
    "docs/SEARCH_API.md",
    "docs/SEARCH_INDEX.md",
]

ADR_DOCUMENTS = [
    "docs/adr/0001-initial-architecture.md",
    "docs/adr/0002-bounded-ingestion-and-persistent-cgb3-archive.md",
    "docs/adr/0003-production-cpi-index.md",
]

ALLOWED_STATUSES = {"current", "reference", "historical", "planned", "superseded"}

IGNORED_DIRECTORIES = {
    ".git",
    "target",
    "node_modules",
    ".idea",
    ".vscode",
    "__pycache__",
}


def strip_fenced_code(source: str) -> str:
    """Removes fenced code blocks (``` ... ``` or ~~~ ... ~~~) from markdown content."""
    lines = source.splitlines()
    visible = []
    fence_char = None
    fence_len = 0

    for line in lines:
        if fence_char:
            m = re.match(r"^ {0,3}(`+|~+)[ \t]*$", line)
            if m and m.group(1)[0] == fence_char and len(m.group(1)) >= fence_len:
                fence_char = None
                fence_len = 0
            visible.append("")
            continue

        m = re.match(r"^ {0,3}(`{3,}|~{3,})", line)
        if m:
            fence_char = m.group(1)[0]
            fence_len = len(m.group(1))
            visible.append("")
            continue

        visible.append(line)

    return "\n".join(visible)


def extract_destination(raw_dest: str) -> str:
    dest = raw_dest.strip()
    if dest.startswith("<"):
        end = dest.find(">")
        return dest[1:end] if end > 0 else ""
    # Strip optional title after space: "path/to/file 'title'"
    match = re.match(r"^(?:\\.|[^\s])+", dest)
    return match.group(0) if match else ""


def resolve_local_target(doc_path: str, raw_dest: str):
    dest = extract_destination(raw_dest)
    if not dest:
        return None
    # Skip external URLs, anchors, mailto, etc.
    if re.match(r"^(?:https?:|mailto:|data:|ftp:|javascript:)", dest, re.IGNORECASE):
        return None
    # Extract file path part before hash / query
    file_part = dest.split("#", 1)[0].split("?", 1)[0]
    if not file_part:
        # Same-file anchor link (e.g. #section)
        return None

    # Handle unescaping
    file_part = re.sub(r"\\([\\ ()])", r"\1", file_part)

    if file_part.startswith("/"):
        full_target = os.path.join(ROOT_DIR, file_part.lstrip("/"))
    else:
        full_target = os.path.normpath(os.path.join(os.path.dirname(doc_path), file_part))

    return full_target, dest


def validate_markdown_file(
    full_path: str,
    link_errors: list,
    image_errors: list,
    metadata_errors: list,
    filename_errors: list,
):
    rel_path = os.path.relpath(full_path, ROOT_DIR)
    is_root = os.path.dirname(rel_path) == ""
    basename = os.path.basename(full_path)
    stem, ext = os.path.splitext(basename)

    if ext != ".md":
        filename_errors.append(f"{rel_path}: extension must be lowercase .md")
        return

    # Check uppercase basename rule for canonical docs and root docs
    is_canonical = rel_path in CANONICAL_DOCUMENTS
    if (is_root or is_canonical) and stem != stem.upper() and not stem.startswith("0"):
        filename_errors.append(f"{rel_path}: Basename must be uppercase")

    with open(full_path, "r", encoding="utf-8") as f:
        content = f.read()

    # Frontmatter validation
    has_frontmatter = content.startswith("---\n") or content.startswith("---\r\n")
    if is_root and has_frontmatter:
        metadata_errors.append(f"{rel_path}: Root Markdown files must NOT contain YAML frontmatter")

    if has_frontmatter:
        match = re.match(r"^---\r?\n(.*?)\r?\n---(?:\r?\n|$)", content, re.DOTALL)
        if not match:
            metadata_errors.append(f"{rel_path}: Unterminated YAML frontmatter")
        else:
            fields = {}
            for line in match.group(1).splitlines():
                fm_match = re.match(r"^([a-z_]+):\s*(.+)$", line)
                if fm_match:
                    k, v = fm_match.group(1), fm_match.group(2).strip().strip('"\'')
                    fields[k] = v

            status = fields.get("status")
            if not status or status not in ALLOWED_STATUSES:
                metadata_errors.append(
                    f"{rel_path}: Missing or invalid 'status' ({status!r}). Allowed: {ALLOWED_STATUSES}"
                )

    # Link validation outside code blocks
    cleaned_source = strip_fenced_code(content)

    # 1. Inline links: [label](target)
    for m in re.finditer(r"(?<!!)\[([^\]]+)\]\(([^)]+)\)", cleaned_source):
        res = resolve_local_target(full_path, m.group(2))
        if res:
            target_path, raw_dest = res
            if not os.path.exists(target_path):
                link_errors.append(f"{rel_path} -> {raw_dest} (Target not found: {os.path.relpath(target_path, ROOT_DIR)})")

    # 2. Images: ![label](target)
    for m in re.finditer(r"!\[([^\]]*)\]\(([^)]+)\)", cleaned_source):
        res = resolve_local_target(full_path, m.group(2))
        if res:
            target_path, raw_dest = res
            if not os.path.exists(target_path):
                image_errors.append(f"{rel_path} -> {raw_dest} (Image target not found: {os.path.relpath(target_path, ROOT_DIR)})")


def main():
    link_errors = []
    image_errors = []
    metadata_errors = []
    filename_errors = []

    reference = os.path.join(ROOT_DIR, "docs", "API_REFERENCE.md")
    if os.path.isfile(reference):
        with open(reference, encoding="utf-8") as file:
            api_reference = file.read()
        for stale_claim in (
            r"the API has no rating filter",
            r"game IDs cannot be discovered over HTTP",
            r"the HTTP API cannot list games",
        ):
            if re.search(stale_claim, api_reference, re.IGNORECASE):
                metadata_errors.append(f"docs/API_REFERENCE.md: stale search claim ({stale_claim})")
        for route in ("/v1/explorer/position/filtered", "/v1/collections/{collection}/games"):
            if route not in api_reference:
                metadata_errors.append(f"docs/API_REFERENCE.md: missing search route ({route})")

    # 1. Walk repository and validate all Markdown files
    for root, dirs, files in os.walk(ROOT_DIR):
        dirs[:] = [d for d in dirs if d not in IGNORED_DIRECTORIES]
        for f in files:
            if f.endswith(".md"):
                full_path = os.path.join(root, f)
                validate_markdown_file(
                    full_path,
                    link_errors,
                    image_errors,
                    metadata_errors,
                    filename_errors,
                )

    # 2. Validate Canonical Documents Existence and Frontmatter
    for canon in CANONICAL_DOCUMENTS:
        full_path = os.path.join(ROOT_DIR, canon)
        if not os.path.exists(full_path):
            metadata_errors.append(f"{canon}: Canonical document is MISSING")
            continue

        with open(full_path, "r", encoding="utf-8") as f:
            raw = f.read()

        match = re.match(r"^---\r?\n(.*?)\r?\n---(?:\r?\n|$)", raw, re.DOTALL)
        if not match:
            metadata_errors.append(f"{canon}: Canonical document missing frontmatter")
            continue

        fields = {}
        for line in match.group(1).splitlines():
            m = re.match(r"^([a-z_]+):\s*(.+)$", line)
            if m:
                fields[m.group(1)] = m.group(2).strip().strip('"\'')

        for req in ["summary", "read_when", "status"]:
            if req not in fields:
                metadata_errors.append(f"{canon}: Missing required frontmatter field: {req}")

        if fields.get("status") != "current":
            metadata_errors.append(f"{canon}: Canonical status must be 'current' (got {fields.get('status')!r})")

    # 3. Validate Canonical Index (docs/README.md) links all canonical documents
    docs_readme = os.path.join(ROOT_DIR, "docs", "README.md")
    if os.path.exists(docs_readme):
        with open(docs_readme, "r", encoding="utf-8") as f:
            index_content = strip_fenced_code(f.read())

        for canon in CANONICAL_DOCUMENTS:
            if canon == "docs/README.md":
                continue
            basename = os.path.basename(canon)
            # Check either relative link from docs/ (e.g. ARCHITECTURE.md or adr/README.md)
            expected_ref = os.path.relpath(os.path.join(ROOT_DIR, canon), os.path.join(ROOT_DIR, "docs"))
            if expected_ref not in index_content and basename not in index_content:
                metadata_errors.append(f"docs/README.md missing canonical link to: {canon}")

    # 4. Validate ADR index (docs/adr/README.md) links all ADRs
    adr_readme = os.path.join(ROOT_DIR, "docs", "adr", "README.md")
    if os.path.exists(adr_readme):
        with open(adr_readme, "r", encoding="utf-8") as f:
            adr_content = strip_fenced_code(f.read())

        for adr in ADR_DOCUMENTS:
            adr_basename = os.path.basename(adr)
            if adr_basename not in adr_content:
                metadata_errors.append(f"docs/adr/README.md missing ADR link to: {adr_basename}")

    # Report results
    failed = False
    if filename_errors:
        failed = True
        print("Filename Errors:\n" + "\n".join(f"  - {e}" for e in filename_errors), file=sys.stderr)

    if metadata_errors:
        failed = True
        print("Metadata / Frontmatter Errors:\n" + "\n".join(f"  - {e}" for e in metadata_errors), file=sys.stderr)

    if link_errors:
        failed = True
        print("Broken Markdown Links:\n" + "\n".join(f"  - {e}" for e in link_errors), file=sys.stderr)

    if image_errors:
        failed = True
        print("Broken Markdown Images:\n" + "\n".join(f"  - {e}" for e in image_errors), file=sys.stderr)

    if not failed:
        print("Documentation check passed: all canonical documents exist, frontmatter valid, and all internal links resolve.")
        sys.exit(0)
    else:
        sys.exit(1)


if __name__ == "__main__":
    main()
