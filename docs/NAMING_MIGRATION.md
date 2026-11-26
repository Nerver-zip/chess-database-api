---
summary: "Upgrade CLI, image and configuration names to Chess Database API."
read_when: "Upgrading a script, container or integration from Opening Explorer API or CODB."
status: current
---

# Naming migration

Chess Database API is the product name. Opening Explorer remains the name of the opening-statistics capability.

| Surface | Current | Compatibility |
| --- | --- | --- |
| CLI | `chess-database` | `opening-explorer` accepts the same commands, arguments and environment. |
| Rust workspace packages | `chess-database-{core,ingest,curate,storage,index,api,cli}` | Use the new Cargo package names and `chess_database_*` imports. |
| Local container image | `chess-database-api:local` | Compose also tags the build as `opening-explorer-api:local`. The image includes the old executable path. |
| Compose project and service | `opening-explorer-api` and `opening-explorer` | Existing volume and container names retain their defaults. |
| Network alias | `chess-database` | `opening-explorer` and `codb` are retained. |
| Server and Compose configuration | `CHESS_DATABASE_*` | Standard CLI flags take precedence. Among environment names, `CHESS_DATABASE_*`, `OPENING_EXPLORER_*`, then `CODB_*` take precedence. |
| GitHub repository | `Nerver-zip/chess-database-api` | GitHub redirects `Nerver-zip/opening-explorer-api`. Existing release tags and asset identities carry over. |

Copy your existing configuration into the upgraded checkout, then run `docker compose config` and verify the rendered mount sources and ports. Change environment prefixes when convenient. The new Compose configuration uses the same default network and volume names and mounts dataset roots read-only. Use isolated projects and networks when running the old and new images at the same time.

The `codb-` dataset version prefix, release IDs, stable game IDs, binary magics and HTTP routes retain their contracts. A CPI-only snapshot still supports the existing Explorer and PGN routes. To enable search, [build the optional indexes](SEARCH_INDEX.md) and reload the resulting immutable snapshot. Keep the older snapshot available and select it before starting an older server binary.

Review the [earlier CODB migration](MIGRATION_FROM_CODB.md) for older deployment names and aliases. The [Docker guide](DOCKER_DEPLOYMENT.md) covers resource limits, networks and mount configuration.


## Rust consumers

The seven previous package names had no entries in the crates.io index. GitHub code search found uses in this repository, and the checked-out Evalis integration consumes HTTP. The workspace therefore renames its internal packages directly. If a downstream project uses a Git or path dependency, update its package field and imports. Cargo supports retaining a local dependency alias through the `package` field if needed.

## Repository rename and post-cutover checks

The repository uses the new slug and retains `main` as the default branch. Both published dataset tags resolve at the new URL. Keep the former slug available for GitHub redirects.

1. Update existing checkouts with `git remote set-url origin https://github.com/Nerver-zip/chess-database-api.git`, then run `git fetch origin`. Update clone links, webhook consumers, badges, and any automation or credential configuration that names the old slug.
2. Review protected branch and tag rules, required checks, and Actions environment policies in GitHub. Check repository, pull request, and release links. Compare published asset SHA-256 metadata with the release manifests.
3. Before application rollout, render and compare the Compose configuration. Keep the existing project name, service key, volume names, and network configuration so the deployment retains its current resource identities.
