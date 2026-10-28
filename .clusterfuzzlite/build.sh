#!/bin/bash -eu
# Builds every target in fuzz/fuzz_targets/ and copies it where ClusterFuzzLite looks for it.
cd "$SRC/kausa"
cargo fuzz build -O
for target in fuzz/fuzz_targets/*.rs; do
  name=$(basename "${target%.*}")
  cp "fuzz/target/x86_64-unknown-linux-gnu/release/$name" "$OUT/"
done
