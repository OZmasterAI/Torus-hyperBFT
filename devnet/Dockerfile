# Fast devnet image — copies pre-built binary from host.
# Requires: cargo build --release -p torus-node
# For full in-Docker build, use Dockerfile.full-build instead.
# Base must carry glibc >= the build host's: binaries come from the ThinkPad
# dev-executor (Ubuntu 26.04, glibc 2.43); ubuntu:24.04 (2.39) cannot load
# them. NOTE the same constraint applies to deploying laptop builds on the
# 24.04 testnet boxes — those still need a VPS-side (or 24.04-container) build.
FROM ubuntu:26.04

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY target/release/torus-node /usr/local/bin/torus-node

# 8645 = RH5 read-only RPC server (read/write isolation).
EXPOSE 30333/udp 8545 8645 9090

ENTRYPOINT ["torus-node"]
