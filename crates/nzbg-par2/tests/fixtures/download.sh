#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

BASE_URL="https://raw.githubusercontent.com/Parchive/par2cmdline/master/tests"

FILES=(
    flatdata.tar.gz
    flatdata-par2files.tar.gz
    subdirdata.tar.gz
    subdirdata-par2files-unix.tar.gz
    readbeyondeof.tar.gz
)

for f in "${FILES[@]}"; do
    if [ ! -f "$f" ]; then
        echo "Downloading $f..."
        curl -sLO "$BASE_URL/$f"
    else
        echo "Already present: $f"
    fi
done

echo "All fixtures ready."
