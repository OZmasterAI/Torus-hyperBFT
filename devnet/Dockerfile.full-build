# Multi-stage build for torus-node
# Stage 1: Build
FROM rust:1.82-slim-bookworm AS builder

# TODO: pin mold version for reproducible release builds
RUN apt-get update && apt-get install -y \
    pkg-config libssl-dev libclang-dev clang cmake mold \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY . .

# Cap cargo parallelism so the workspace release build fits comfortably in
# ~4 GB of RAM per rustc process (host has limited free RAM).
ARG CARGO_BUILD_JOBS=3
ENV CARGO_BUILD_JOBS=$CARGO_BUILD_JOBS
RUN cargo build --release --bin torus-node

# Stage 2: Runtime
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/torus-node /usr/local/bin/torus-node

EXPOSE 30333/udp 8545 9090

ENTRYPOINT ["torus-node"]
