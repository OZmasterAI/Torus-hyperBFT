#!/usr/bin/env bash
# Start a 4-node Torus devnet using Docker Compose.
#
# Usage:
#   ./start.sh          # build and start
#   ./start.sh --build  # force rebuild
#   ./start.sh down     # stop and remove containers
#   ./start.sh logs     # follow logs

set -euo pipefail
cd "$(dirname "$0")"

case "${1:-up}" in
    up)
        echo "Starting 4-node Torus devnet..."
        docker compose up -d --build
        echo ""
        echo "Devnet is running:"
        echo "  Validator 0  RPC: http://localhost:8545  P2P: 30333/udp"
        echo "  Validator 1  RPC: http://localhost:8546  P2P: 30334/udp"
        echo "  Validator 2  RPC: http://localhost:8547  P2P: 30335/udp"
        echo "  Validator 3  RPC: http://localhost:8548  P2P: 30336/udp"
        echo "  Metrics:     http://localhost:9090/metrics"
        echo ""
        echo "Use './start.sh logs' to follow output."
        ;;
    --build)
        echo "Rebuilding and starting devnet..."
        docker compose up -d --build --force-recreate
        ;;
    down)
        echo "Stopping devnet..."
        docker compose down -v
        ;;
    logs)
        docker compose logs -f
        ;;
    *)
        echo "Usage: $0 [up|--build|down|logs]"
        exit 1
        ;;
esac
