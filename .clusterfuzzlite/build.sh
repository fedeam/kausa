#!/bin/bash -eu
# Builds every target in fuzz/fuzz_targets/ and copies it where ClusterFuzzLite looks for it.
cd "$SRC/kausa"
cargo fuzz build -O
for target in fuzz/fuzz_targets/*.rs; do
  name=$(basename "${target%.*}")
  cp "fuzz/target/x86_64-unknown-linux-gnu/release/$name" "$OUT/"
done
# The seeds of a target, fuzz/seeds/<target>/ (fake values only), go next to its binary as a flat
# zip named <target>_seed_corpus.zip, the name OSS-Fuzz documents for a seed corpus.
for seeds in fuzz/seeds/*/; do
  [ -d "$seeds" ] || continue
  name=$(basename "$seeds")
  zip -j -q "$OUT/${name}_seed_corpus.zip" "$seeds"*
done
