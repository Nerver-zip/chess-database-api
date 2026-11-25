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
| GitHub repository | `Nerver-zip/opening-explorer-api` until owner rename | Future slug `Nerver-zip/chess-database-api`. Published release links remain on the current slug until cutover. |

Copy your existing configuration into the upgraded checkout, then run `docker compose config` and verify the rendered mount sources and ports. Change environment prefixes when convenient. The new Compose configuration uses the same default network and volume names and mounts dataset roots read-only. Use isolated projects and networks when running the old and new images at the same time.

The `codb-` dataset version prefix, release IDs, stable game IDs, binary magics and HTTP routes retain their contracts. A CPI-only snapshot still supports the existing Explorer and PGN routes. To enable search, [build the optional indexes](SEARCH_INDEX.md) and reload the resulting immutable snapshot. Keep the older snapshot available and select it before starting an older server binary.

Review the [earlier CODB migration](MIGRATION_FROM_CODB.md) for older deployment names and aliases. The [Docker guide](DOCKER_DEPLOYMENT.md) covers resource limits, networks and mount configuration.


## Rust consumers

The seven previous package names had no entries in the crates.io index. GitHub code search found uses in this repository, and the checked-out Evalis integration consumes HTTP. The workspace therefore renames its internal packages directly. If a downstream project uses a Git or path dependency, update its package field and imports. Cargo supports retaining a local dependency alias through the `package` field if needed.

## Repository rename after merge

1. An owner confirms PR #11 is merged and the target slug `chess-database-api` is available. Record the existing default branch, protected branch and tag rules, Actions environment policies, release tags, and any deployment or bot links.
2. In GitHub **Settings → General**, rename the repository to `chess-database-api`. Current Cargo metadata already points at the future slug. Keep the old slug available for GitHub redirects.
3. Update existing checkouts with `git remote set-url origin https://github.com/Nerver-zip/chess-database-api.git`, then run `git fetch origin`. Update current clone links, webhook consumers, badges, and any hard-coded automation or credential configuration that names the old slug.
4. Verify that the new repository, default branch, required checks and tag protections are visible. Check that release tags and published assets resolve under the new slug and compare their recorded checksums. Check redirects for the original repository, draft or merged PR, and published release URLs. Update active links if the redirect does not cover them.
5. Before any application rollout, render and compare the deployment configuration. Keep the existing Compose project name, service key, volume names and network configuration. The GitHub rename is independent of container and dataset identity.

The current installation remains on the original GitHub slug until the owner performs the rename.
