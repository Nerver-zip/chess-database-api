---
summary: "Historical CODB migration and retained compatibility identifiers."
read_when: "Upgrading scripts, deployments, or environments created before the opening-explorer-api rename, or deciding whether an old identifier is still valid."
status: current
---

# Migration from CODB

The project was previously called **CODB** (repository `chess-openings-db`). The Nov. 2026 migration renamed it to **Opening Explorer API**. This guide records that historical transition. For the current Chess Database API names and upgrade procedure, see [Naming migration](NAMING_MIGRATION.md).

| Scope | Name after the first migration |
|---|---|
| Repository and service | `opening-explorer-api` |
| CLI binary and public command | `opening-explorer` |
| Docker image | `opening-explorer-api:local` |

That migration preserved CLI commands and arguments, HTTP routes and error types. Stored datasets, release manifests, and game and position identifiers kept their existing contracts.

## Names

| Area | CODB name | Name after the first migration | Compatibility at that transition |
|---|---|---|---|
| CLI binary | `codb` | `opening-explorer` | No second binary is shipped. Update scripts, PATH entries, and `docker exec` commands. |
| Cargo packages | `codb-core`, `codb-ingest`, `codb-curate`, `codb-storage`, `codb-index`, `codb-api`, `codb-cli` | `opening-explorer-core`, `-ingest`, `-curate`, `-storage`, `-index`, `-api`, `-cli` | Rust import paths followed the package names, such as `opening_explorer_core`. |
| Repository | `Nerver-zip/chess-openings-db` | `Nerver-zip/opening-explorer-api` | GitHub redirects the previous slug. |
| Docker image | `codb-opening-explorer:local` | `opening-explorer-api:local` | Built by Compose from the same `Dockerfile`. Existing local images are not removed by the rename. |
| Container user and binary path | user `codb`, `/usr/local/bin/codb` | user `opening-explorer`, `/usr/local/bin/opening-explorer` | UID and GID stay `10001`, so existing volume ownership still matches. |
| Compose file | `compose.codb.yaml` | `compose.yaml` | Same services and overrides under the new names. |
| Compose override | `compose.codb.named-volume.yaml` | `compose.named-volume.yaml` | Same named-volume mode. |
| Compose override | `compose.codb.debug.yaml` | `compose.debug.yaml` | Same loopback debug port. |
| Compose service | `codb` | `opening-explorer` | Compose project default changed from `chess-openings-db` to `opening-explorer-api`. |
| Shared network alias | `codb` | `opening-explorer` (and `codb`) | `codb` remains a deprecated alias for sibling containers. |
| Default shared network | `codb-shared` | `opening-explorer-shared` | Set `OPENING_EXPLORER_NETWORK_NAME` to keep an existing name. |
| Example settings | `.env.codb.example` | `.env.example` | Same settings under new names. |
| Named-volume example settings | `.env.codb.gigabase.example` | `.env.gigabase.example` | Keeps the original R3 resource names (see below). |

## Environment variables

The first migration introduced corresponding `OPENING_EXPLORER_*` names. The [current precedence and configuration](NAMING_MIGRATION.md) cover `CHESS_DATABASE_*` and the retained fallbacks.

| CODB variable | Name introduced in the first migration | Compatibility at that transition |
|---|---|---|
| `CODB_MASTERS_DATASET`, `CODB_ELITE_DATASET`, `CODB_HOST`, `CODB_PORT`, `CODB_ALLOWED_ORIGINS`, `CODB_REQUEST_TIMEOUT_SECS`, `CODB_MAX_MOVE_PLIES`, `CODB_READER_POOL_SIZE`, `CODB_READER_POOL_TIMEOUT_SECS`, `CODB_ENABLE_GAME_RETRIEVAL`, `CODB_ADMIN_TOKEN` | `OPENING_EXPLORER_` + same suffix | `opening-explorer serve`. The CLI prints a deprecation warning to stderr when it promotes a legacy name. |
| `CODB_DATA_DIR`, `CODB_MASTERS_RELEASE_DIR`, `CODB_ELITE_RELEASE_DIR`, `CODB_NETWORK_NAME`, `CODB_NETWORK_EXTERNAL`, `CODB_RUNTIME_VOLUME`, `CODB_READER_POOL_SIZE`, `CODB_READER_POOL_TIMEOUT_SECS`, `CODB_REQUEST_TIMEOUT_SECS`, `CODB_MAX_MOVE_PLIES`, `CODB_ENABLE_GAME_RETRIEVAL`, `CODB_ALLOWED_ORIGINS`, `CODB_MEMORY_LIMIT`, `CODB_CPU_LIMIT`, `CODB_LOG_LEVEL`, `CODB_DEBUG_PORT` | `OPENING_EXPLORER_` + same suffix | Compose files, read through nested interpolation. Existing `.env` files keep working. |
| `CODB_DATA_DIR`, `CODB_BIN`, `CODB_CORPUS_ALLOW_HTTP` | `OPENING_EXPLORER_DATA_DIR`, `OPENING_EXPLORER_BIN`, `OPENING_EXPLORER_CORPUS_ALLOW_HTTP` | `scripts/corpus.py` and `scripts/corpus.sh` |
| `CODB_DATA_DIR`, `CODB_FIXTURES_DIR` | `OPENING_EXPLORER_DATA_DIR`, `OPENING_EXPLORER_FIXTURES_DIR` | `scripts/reproduce_benchmarks.sh` |
| `CODB_BENCH_WORKDIR`, `CODB_MASTERS_FILE`, `CODB_ELITE_FILE` | `OPENING_EXPLORER_BENCH_WORKDIR`, `OPENING_EXPLORER_MASTERS_FILE`, `OPENING_EXPLORER_ELITE_FILE` | `scripts/bench_production_cpi.py`. Renamed without a fallback. |
| `CODB_CRASH_AT`, `CODB_TEST_SUBPROCESS_CRASH`, `CODB_TEST_CHILD_TARGET` | `OPENING_EXPLORER_CRASH_AT`, `OPENING_EXPLORER_TEST_SUBPROCESS_CRASH`, `OPENING_EXPLORER_TEST_CHILD_TARGET` | Fault-injection hooks used by tests. Renamed without a fallback. |

The Rust `RUST_LOG` filter is unchanged.

## Identifiers that intentionally keep the old spelling

These are part of stored data, published evidence, or external references. Changing them would break existing datasets, pinned checksums, or the history of the project.

| Identifier | Where it appears | Why it stays |
|---|---|---|
| `codb-` prefix on generated build and release IDs (for example `codb-masters-otb-1794774904566910568-6701-5698b67c3952ca6b-cpi-d40`) | Dataset manifests, `versions/` directories, `RELEASE.json`, package names | Immutable release identifiers. New builds keep the prefix (`BUILD_ID_PREFIX` in `crates/storage/src/dataset.rs`). |
| `CGB3` block magic and `shard_XXXX.cgb3` names; `CGCS` / `CGCE` shard markers; `CGL1` lookup magic | Shard, shard-footer, and `games.idx` / lookup files | Binary layout contract. |
| `CODB_CPI` magic | `positions.cpi` (CPI v2) | Binary layout contract. |
| `CODB_RUN` magic | Temporary `.crun` run files | Binary layout contract. |
| `codb_block_compressed_bytes`, `codb_block_directory_bytes`, `codb_total_container_bytes`, `codb_bytes_per_game` | `bench` JSON output and the historical benchmark files | Serialized schema keys that the benchmark validators read. |
| `codb_version` | `RELEASE.json` provenance | Key in pinned release metadata. |
| Game IDs, CPI keys, collection names, HTTP routes, error types | API and storage | Compatibility contracts. |
| Historical evidence and history | `benchmarks/`, `docs/research/`, `docs/adr/` history, `handoff/`, the older `CHANGELOG.md` entries | Recorded artifacts. Commands inside them use the legacy `codb` name and describe the build that produced them. |

## Existing Gigabase R3 (named-volume) deployment

The existing R3 deployment keeps its Docker resources, so redeploying does not create a new empty runtime volume:

- Named volume `codb-gigabase-r3-runtime` (set `OPENING_EXPLORER_RUNTIME_VOLUME` to change it).
- Network `codb-gigabase-r3-net` (set `OPENING_EXPLORER_NETWORK_NAME` to change it).
- Compose project `codb-gigabase-r3`.

The running container `codb-gigabase-r3-codb-1` and its image `codb-opening-explorer:local` were not changed. The redeploy under the new service name creates `codb-gigabase-r3-opening-explorer-1`. **Stop the old container first.** Otherwise both containers answer the shared alias `codb` on the same network.

## Checking the historical migration

```bash
# The CLI name and its subcommands.
cargo build --locked --release --package chess-database-cli
target/release/chess-database --help

# Compose resolves both new and legacy variables.
docker compose -f compose.yaml --env-file .env.example config --quiet
```

To confirm the retained port fallback, leave `CHESS_DATABASE_PORT` and `OPENING_EXPLORER_PORT` unset, then run `CODB_PORT=8081 target/release/chess-database serve --help`.
