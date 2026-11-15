# Contributing to `chess-openings-db`

Thank you for contributing to `chess-openings-db`! This document outlines our contribution workflow, coding standards, and verification expectations.

The project is **English-only** for code comments, user-facing strings, documentation, Pull Requests, and commit messages. Please read [AGENTS.md](AGENTS.md), [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), and [docs/DATA_CONTRACTS.md](docs/DATA_CONTRACTS.md) before modifying ingestion, storage, or indexing logic.

---

## 1. Quick Verification

To verify the repository locally before opening a Pull Request:

```bash
# Unified verification entrypoint (runs fmt, clippy, cargo test, python tests, doc validation)
./scripts/validate.sh
```

Individual checks can also be run directly:
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python3 -m unittest scripts/test_feasibility_alignment.py \
                    scripts/test_reproduce_runner.py \
                    scripts/test_validate_benchmarks.py \
                    scripts/test_bench_http.py \
                    scripts/test_corpus.py
python3 scripts/validate_production_cpi_benchmarks.py
python3 scripts/validate_corpus_evidence.py
python3 scripts/validate_benchmarks.py
python3 scripts/validate_docs.py
sha256sum -c MANIFEST.sha256
```

---

## 2. Contribution Guidelines & PR Expectations

1. **Focused Changes:** Keep pull requests small and focused. Always include tests alongside implementation changes.
2. **Preserve Binary Contracts:** Never modify bit-packing in `CompactMove`, container magic or block structures in `CGB3`, fixed-record layouts in `games.idx`, or binary index layouts in `CPI v2` without an explicit Architecture Decision Record (ADR) and backward compatibility migration.
3. **Fail-Closed Accounting:** Any changes to ingestion or curation must preserve the strict accounting invariant:
   $$\text{total\_encountered} = \text{accepted\_unique} + \text{confirmed\_duplicates} + \text{superseded\_prior} + \text{rejected\_by\_policy} + \text{rejected\_variant} + \text{rejected\_illegal} + \text{rejected\_malformed} + \text{quarantined}$$
   and $\text{retained\_canonical\_games} == \text{accepted\_unique}$.
4. **Zero AGPL Code Reuse:** Do not copy, translate, or import code from `lila-openingexplorer` or any other AGPL-3.0 codebase. All code must be independently authored under the project's dual license ([MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE)).
5. **No Large Datasets in Git:** Never commit large external PGN archives, build output directories (`data/builds/`), or transient run files (`.crun`) into Git. Only small deterministic test fixtures belong in `fixtures/`.
6. **Documentation Updates:** Update the corresponding canonical document in `docs/` when system behavior, CLI arguments, or storage layouts change. Run `python3 scripts/validate_docs.py` to ensure internal links and frontmatter remain valid.
7. **Handoff Log:** Update [HANDOFF.md](HANDOFF.md) with work summary, verification status, and next steps before finishing your turn.
