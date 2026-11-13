#!/usr/bin/env bash
# Public end-to-end check: committed DP-fitted spec -> synthetic data -> equivalence + privacy report.
# Needs: cargo, python3 with `pip install .[eval]` (numpy, scipy, scikit-learn).
set -euo pipefail
cd "$(dirname "$0")"
ROOT=../..
./fetch.sh >/dev/null
python3 prepare.py
cargo build --release --manifest-path $ROOT/Cargo.toml --bin sdg-rs -q
rm -rf work/synth
$ROOT/target/release/sdg-rs schema adult.fitted.graphql --out work/synth --seed 1 --format csv >/dev/null
PYTHONPATH=$ROOT/src python3 -m sdg.evaluate \
  --train work/train.csv --holdout work/holdout.csv --synth work/synth/Adult.csv \
  --domains work/domains.json --label income --ledger ledger.json --seed 20261113 \
  --out work/report
cp work/report.md report.md
