# The image ClusterFuzzLite builds the fuzz targets in: OSS-Fuzz's Rust builder, with a nightly
# compiler and cargo-fuzz, pinned by the digest of its index.
FROM gcr.io/oss-fuzz-base/base-builder-rust@sha256:3428d0a90b1e7efbe2ba947bd1d0c9a70594986b4b6553804547f1fb24cfc92f
COPY . $SRC/kausa
WORKDIR $SRC/kausa
COPY .clusterfuzzlite/build.sh $SRC/
