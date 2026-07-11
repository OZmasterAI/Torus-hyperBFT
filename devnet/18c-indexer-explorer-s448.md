# 18c — indexer + block explorer (S448)

18c hosts BOTH the Rust indexer (`torus-explorer`) and the Next.js block explorer
(`torus-HBFT-explorer`). Both read the **local 18c validator RPC on
`127.0.0.1:8555`**. Start them only AFTER the chain is live.

Ports:
- `8555` — 18c validator JSON-RPC + WS (loopback)
- `3001` — Rust indexer REST API (`/api/*`)
- `3000` — Next.js explorer UI (expose this publicly via reverse proxy)

Everything is pre-staged:
- Rust indexer binary: `/home/18c/torus-hyperbft/target/release/torus-explorer`
- Frontend repo:        `/home/18c/torus-hbft-explorer` (built, `.env.local` set → 8555)

---

## 1. Rust indexer (`torus-explorer`)

Backfills genesis→head, follows new heads over WS, serves `/api/*` from SQLite.

```bash
/home/18c/torus-hyperbft/target/release/torus-explorer \
  --rpc-url http://127.0.0.1:8555 \
  --ws-url  ws://127.0.0.1:8555 \
  --db-path /home/18c/explorer.db \
  --listen  0.0.0.0:3001
```

## 2. Next.js explorer UI

`.env.local` is already written (RPC/WS → 8555, API → the Rust indexer on 3001).

```bash
cd /home/18c/torus-hbft-explorer
pnpm start            # serves the UI on :3000
```

## 3. Durable setup (systemd) — recommended

Two user/system services so they survive reboots and restart on crash. Example
units (install under `/etc/systemd/system/`, `sudo systemctl enable --now` once
the chain is live):

```ini
# /etc/systemd/system/torus-indexer.service
[Unit]
Description=Torus explorer indexer (torus-explorer)
After=network.target torus-hbft-validator.service
[Service]
User=18c
ExecStart=/home/18c/torus-hyperbft/target/release/torus-explorer \
  --rpc-url http://127.0.0.1:8555 --ws-url ws://127.0.0.1:8555 \
  --db-path /home/18c/explorer.db --listen 0.0.0.0:3001
Restart=always
RestartSec=10
[Install]
WantedBy=multi-user.target
```

```ini
# /etc/systemd/system/torus-explorer-ui.service
[Unit]
Description=Torus block explorer UI (Next.js)
After=network.target torus-indexer.service
[Service]
User=18c
WorkingDirectory=/home/18c/torus-hbft-explorer
ExecStart=/usr/bin/env pnpm start
Environment=PORT=3000
Restart=always
RestartSec=10
[Install]
WantedBy=multi-user.target
```

## Exposure
- Keep validator RPC (`8555`) loopback — do not expose.
- Publish only the UI (`3000`) via nginx/caddy; optionally the indexer API (`3001`).

## Notes
- Indexer is light (≈1 core, sub-GB RAM); it may lag the tip during bench bursts
  and catches up after — it won't stress this 18-core/94G box.
- `explorer.db` grows with history; 18c has ~246G free — plenty of runway.
- If the frontend errors on a native module, run
  `pnpm rebuild keccak bufferutil utf-8-validate sharp` in the repo.
