#!/usr/bin/env bash
# Download UCI Adult (CC BY 4.0, https://archive.ics.uci.edu/dataset/2/adult). Not vendored.
set -euo pipefail
cd "$(dirname "$0")"
mkdir -p data && cd data
if [ ! -f adult.data ]; then
  curl -fsSL -o adult.zip https://archive.ics.uci.edu/static/public/2/adult.zip
  python3 -c "import zipfile;zipfile.ZipFile('adult.zip').extractall('.')"
fi
sha256sum adult.data adult.test | tee SHA256SUMS
