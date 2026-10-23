#!/usr/bin/env bash
set -euo pipefail

DATA_DIR="data"
mkdir -p "$DATA_DIR"

echo "=== Downloading Masters OTB and Elite Online Reference Datasets ==="

# 1. Masters OTB: Lichess Official Broadcasts (CC BY-SA 4.0)
OTB_URL_1="https://database.lichess.org/broadcast/lichess_db_broadcast_2026-09.pgn.zst"
OTB_URL_2="https://database.lichess.org/broadcast/lichess_db_broadcast_2026-08.pgn.zst"
OTB_URL_3="https://database.lichess.org/broadcast/lichess_db_broadcast_2026-07.pgn.zst"

OTB_FILE_1="$DATA_DIR/masters_otb_2026-09.pgn.zst"
OTB_FILE_2="$DATA_DIR/masters_otb_2026-08.pgn.zst"
OTB_FILE_3="$DATA_DIR/masters_otb_2026-07.pgn.zst"

if [ ! -f "$OTB_FILE_1" ]; then
    echo "Downloading Masters OTB 2026-09 (~18 MB)..."
    curl -L -o "$OTB_FILE_1" "$OTB_URL_1"
fi

if [ ! -f "$OTB_FILE_2" ]; then
    echo "Downloading Masters OTB 2026-08 (~31 MB)..."
    curl -L -o "$OTB_FILE_2" "$OTB_URL_2"
fi

if [ ! -f "$OTB_FILE_3" ]; then
    echo "Downloading Masters OTB 2026-07 (~24 MB)..."
    curl -L -o "$OTB_FILE_3" "$OTB_URL_3"
fi

# 2. Elite Online: Lichess Standard Rated Games (CC0 1.0)
ONLINE_URL_1="https://database.lichess.org/standard/lichess_db_standard_rated_2013-01.pgn.zst"
ONLINE_FILE_1="$DATA_DIR/elite_online_2013-01.pgn.zst"

if [ ! -f "$ONLINE_FILE_1" ]; then
    echo "Downloading Elite Online 2013-01 (~17 MB)..."
    curl -L -o "$ONLINE_FILE_1" "$ONLINE_URL_1"
fi

echo "=== Downloads complete in $DATA_DIR ==="
ls -lh "$DATA_DIR"
