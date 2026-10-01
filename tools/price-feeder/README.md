# price-feeder: validator oracle price feeder

One feeder runs per validator. Every `interval_ms` (default 3 s) it fetches
spot best bid/ask from seven venues, computes a weighted median per configured
market, and submits `SubmitOraclePrices` to the validator's local node.

* It signs with the validator's hot oracle signer: a separate address that
  can only submit oracle prices on that validator's behalf. The validator key
  never leaves the validator.
* The chain aggregates the submissions. It needs >= 3 reporters holding
  > 2/3 of Active stake, uses a 10 s window, and marks a price stale after 60 s
  (`docs/plans/oracle-aggregation.md`).

Design: `docs/plans/oracle-feeder.md`.

## Price rules

* Venues and HL weights: Binance 3, OKX 2, Bybit 2, Kraken 1, KuCoin 1, Gate 1,
  MEXC 1.
* Each venue's mid is `(bid + ask) / 2`. A sample counts while it is at most
  5 s old.
* A market is submitted only if it has >= 3 sources and >= 50 % of its
  configured weight. Its price is the lower weighted median of those sources.
* USDT and USDC count as USD 1:1 (`quote_mode = "par"`). With
  `quote_mode = "kraken_usdt"`, USDT mids are converted with Kraken's USDT/USD
  mid instead.
* Only markets that are listed on chain and have a valid price are sent. The
  feeder never sends a submission that the chain would reject.

## Runbook

1. Create the signer keystore on the feeder host:

   ```
   price-feeder keygen --keystore /etc/torus/price-feeder/signer.keystore \
       --passphrase-file /etc/torus/price-feeder/passphrase
   ```

   It prints the signer address and the registration command. A hex key file
   with mode 0600 (`signer_key_file`) also works. Use a fresh, unfunded
   address.

2. Register the signer once, with the validator's EVM keystore (cold):

   ```
   torus-wallet --keystore <validator EVM keystore> set-oracle-signer --signer <signer address>
   ```

   `--clear` removes the signer. A signer can serve only one validator, and it
   cannot itself be a validator.

3. Write the config from `feeder.example.toml`. The MATIC market uses the
   venues' POL tickers, but `base_asset` stays `"MATIC"`, the on-chain name.

4. Check the setup. This never submits:

   ```
   price-feeder check --config /etc/torus/price-feeder/feeder.toml
   ```

   It verifies that the validator lists this signer and is active, and that
   every listed configured market has the same base asset with a USD quote. It
   then prints every venue's status and the price or omission reason for each
   market, including the symbols missing per venue.

5. Install the service. The RPC must be the local node, on loopback:

   ```
   [Unit]
   Description=torus price feeder
   After=network-online.target torus-node.service

   [Service]
   ExecStart=/usr/local/bin/price-feeder run --config /etc/torus/price-feeder/feeder.toml
   Restart=always
   RestartSec=5
   User=torus-feeder

   [Install]
   WantedBy=multi-user.target
   ```

6. Watch `http://127.0.0.1:9466/health` and `/metrics`.
   * `/health` returns `ok`, `degraded` (a market omitted or a venue failing)
     or `down` with HTTP 503. Down means no accepted submission for
     3 x interval, or the node rejected the signer as not authorized.
   * The response also shows each market's last price, sources, weight and
     omission reason, and each venue's status, latency, last error and backoff.
   * Metrics: `feeder_cycles_total`, `feeder_submit_total{result}`,
     `feeder_source_errors_total{exchange}`,
     `feeder_market_omitted_total{market,reason}`,
     `feeder_last_submit_ok_unixtime_ms`.

7. Rotate the signer:
   1. Create a new keystore (step 1).
   2. Run `set-oracle-signer --signer <new>` (step 2).
   3. Point the config at the new keystore and restart the feeder.

   Submissions from the old signer still count in the block that carries the
   rotation, and are rejected from the next block on.

## Behaviour

* **Validator not active.** The feeder idles and re-checks every 60 s.
* **Signer not registered.** `run` and `check` fail at startup and print the
  registration command. While the feeder runs, it idles until the signer is
  registered again.
* **Venue fails.** The venue backs off for `min(1 s x 2^(k-1), 60 s)`. Its last
  good quotes age out after 5 s.
* **Node busy, or the pending cap is reached.** The cap is 4 pending
  submissions per validator. The next cycle retries with a new nonce.
* **Stops.** If the feeder stops, the chain's mark for this validator's
  markets goes stale 60 s after the last quorum.
