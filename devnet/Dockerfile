# Fast devnet image — copies pre-built binary from host.
# Requires: cargo build --release -p torus-node
# For full in-Docker build, use Dockerfile.full-build instead.
FROM ubuntu:24.04

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY target/release/torus-node /usr/local/bin/torus-node

EXPOSE 30333/udp 8545 9090

ENTRYPOINT ["torus-node"]
