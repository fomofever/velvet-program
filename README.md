# Velvet program (FairLaunch)

The on-chain program behind [Velvet](https://velvetlaunch.xyz), the anti-snipe Solana launchpad.
This repository holds only the program source, so anyone can check that the code deployed on-chain
is exactly this code.

| | |
|---|---|
| Program ID | `8f9XtMzn9J8vapxoLoypKUjNta64jhjfr18WMVCn5yAk` |
| Framework | Anchor 0.31.1 |
| Library name | `fairlaunch` |
| Toolchain | Agave 2.2.x (`cargo-build-sbf`, platform-tools v1.48) |

## What it does

- `programs/fairlaunch/src/lib.rs` — instructions: create_launch, create_launch_signed (free
  launch: the creator's Ed25519 approval is checked on-chain, the first buyer pays the rent), commit, settle, claim,
  claim_vested, refund_creator, buy, sell, distribute_rewards, prepare_graduation, graduate,
  lock_liquidity, collect_pool_fees, plus admin: init_config, propose_config / apply_config /
  cancel_config (timelocked), disable_distributor, transfer_authority / accept_authority,
  set_storage (the per-coin permanent-storage fee, capped at 0.001 SOL in code)
- `idl/velvet_launch.json` — the program's interface (Anchor IDL), also published on-chain so
  explorers decode Velvet instructions
- `math.rs` — bonding curve, fee decay, fee split, vesting (unit tested)
- `raydium.rs` — CPIs into Raydium CPMM and the Raydium liquidity locker
- `state.rs`, `errors.rs` — accounts and error codes
- `tests-litesvm/` — end-to-end tests against real Raydium, locker and Metaplex bytecode
  (`./fetch-fixtures.sh` dumps them from devnet), plus a fuzz campaign

Every coin is minted by the program with a fixed 1B supply and minting switched off in the same
transaction. Openings are a single-price batch (the "line"), early flippers pay a decaying cover
charge that goes to holders, and bonded coins move to a Raydium pool with 100% of the LP locked
in Raydium's locker. While bonding, no wallet can get more than 3% of the supply: curve buys, line commits and the
creator's opening buy are all checked. A
quarter of Velvet's share of the pool fees goes back into the pool as liquidity whose LP tokens
are burned. Fee and limit changes wait out an on-chain timelock.

## Verify the deployed program

Velvet uses [Solana verified builds](https://solana.com/docs/programs/verified-builds). With
Docker running and `cargo install solana-verify`:

```sh
# Rebuild from source in the official deterministic Docker image and compare with the chain
solana-verify verify-from-repo -u https://api.mainnet-beta.solana.com \
  --program-id 8f9XtMzn9J8vapxoLoypKUjNta64jhjfr18WMVCn5yAk \
  https://github.com/fomofever/velvet-program \
  --library-name fairlaunch --mount-path . --commit-hash <commit>

# Or just compare hashes yourself
solana-verify build --library-name fairlaunch
solana-verify get-executable-hash target/deploy/fairlaunch.so
solana-verify get-program-hash -u <cluster-url> 8f9XtMzn9J8vapxoLoypKUjNta64jhjfr18WMVCn5yAk
```

The two hashes match when the deployed program was built from that commit. Use
`-u https://api.devnet.solana.com` for the devnet deployment.

Verification proves the deployed code matches this source. It is not an audit.

## Tests

```sh
cargo test -p fairlaunch            # unit tests (math)
cd tests-litesvm && ./fetch-fixtures.sh && cargo test --release
```

## Security

Please report vulnerabilities privately to team@velvetlaunch.xyz rather than opening a public issue.

## Velvet Pools (`programs/velvet-pools`)

The pools coins bond into: one locked SOL/coin pool per bonded coin, created only by the launchpad,
with a 0.35% swap fee charged in SOL (0.20% stays in the pool; 0.15% goes in equal parts to the
coin's holders, its creator and Velvet). No LP tokens and no withdraw: the liquidity is permanent.
Program ID `VPkPeiAg7LheTtDdBByCmXewabF9zF6yK5YYLmfPjWb`.

Derived from [raydium-io/raydium-cp-swap](https://github.com/raydium-io/raydium-cp-swap) at commit
`0f86d46`, under the Apache License 2.0 (see `programs/velvet-pools/LICENSE`). Modified files carry a
notice at the top.

