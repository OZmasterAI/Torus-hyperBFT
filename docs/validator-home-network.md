# Running a validator from home (behind a router)

For testnet validators on a home connection (for example ozarchy). Do the steps in order and stop at the
first one that works.

The node talks to other validators over **UDP 30333** (QUIC). Default listen address:
`--p2p-listen /ip4/0.0.0.0/udp/30333/quic-v1`.

## Step 0: check for carrier-grade NAT (2 minutes)

With carrier-grade NAT (CGNAT), your ISP puts many customers behind one shared public IP. Your router
never gets its own public IP, so port forwarding on it cannot work.

1. Open the router's admin page and find its **WAN / Internet IP** (status page).
2. On ozarchy run: `curl -4 ifconfig.me`
3. Compare:
   - **Same IP**: no CGNAT. Go to Option A.
   - **Different IP**, or a WAN IP starting with `100.64.`-`100.127.`, `10.`, `172.16.`-`172.31.`,
     `192.168.`: CGNAT. Go to Option B (or ask the ISP for a public IPv4; some do it free).

## Option A: port forwarding (preferred)

Every validator can reach you, and connections recover by themselves after a drop.

1. **Fixed LAN IP for ozarchy**: in the router, add a DHCP reservation for ozarchy's MAC address
   (for example `192.168.1.50`). Otherwise the forward breaks when the LAN IP changes.
2. **Forward the port**: router → Port Forwarding / Virtual Server / NAT:
   - external port **30333**, protocol **UDP**, internal IP = ozarchy's LAN IP, internal port **30333**.
3. **Open ozarchy's firewall**, if one is active:
   - ufw: `sudo ufw allow 30333/udp`
   - firewalld: `sudo firewall-cmd --permanent --add-port=30333/udp && sudo firewall-cmd --reload`
4. **Dynamic DNS** (a home IP can change, no static IP needed):
   - Get a free name, e.g. DuckDNS (`archy.duckdns.org`) or No-IP, or use the router's built-in DDNS.
   - Keep it updated (router DDNS setting, or the provider's cron script on ozarchy).
   - Check: `getent hosts archy.duckdns.org` returns your current public IP (`curl -4 ifconfig.me`).
5. **Send 18c**: the DNS name, the port (30333), and your validator public key. The other validators
   then dial you with:
   `/dns4/archy.duckdns.org/udp/30333/quic-v1/p2p/<your peer id>`
   (the node accepts `/dns4/` addresses; your peer id comes from your validator key, 18c derives it).
6. **Your own `--p2p-peers`**: list the public validators 18c sends you, as usual.

If your IP changes while the chain runs, connections drop for a moment. You dial out again and the others
re-resolve the DNS name when they redial, so it recovers by itself.

## Option B: dial-out only (CGNAT, or no access to the router)

Use this when Option A is impossible:
- **CGNAT** (step 0 said so), or
- **No router access**: the ISP locked the router, a landlord or building controls it, or nobody has
  the admin password.

You open the connections to the public validators; nobody connects to you. Every connection carries
traffic both ways, so this works fully, **as long as you are the only validator behind NAT** (two
NAT'd validators cannot reach each other).

1. Nothing to set up on the router or firewall (outbound UDP is allowed by default on home routers).
2. Set `--p2p-peers` to **all** public validators (18c sends the list).
3. Tell 18c you are dial-out: your address goes into nobody's `--p2p-peers`.

Downside: only you can reconnect after a drop (your internet blips, router restarts). The node redials by
itself; a long outage just means you are offline until it does.

## Either way

- **Uptime**: the chain tolerates only 1 of 4 validators down. Turn off sleep and suspend, and schedule
  OS updates and reboots, telling the others first.
- **Shared machine**: benches, builds and the validator compete for CPU and disk. Don't run heavy benches
  or builds while the validator is in the active set.
- **Upload bandwidth**: when you are leader you send each block to every validator. A slow upload
  (below about 50 Mbit/s) makes your leader turns slow for the whole chain. Check with any speed test.
- **Keys**: keep `validator.keystore` private and backed up offline. Nobody else needs it.
