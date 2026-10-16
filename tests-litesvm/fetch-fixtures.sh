#!/usr/bin/env sh
# Dump the real devnet programs/accounts the v2 tests run against.
set -e
cd "$(dirname "$0")/fixtures"
solana program dump -ud DRaycpLY18LhpbydsBWbVJtxpNv9oXPgjRSfpF2bWpYb raydium_cpmm.so
solana program dump -ud DRay25Usp3YJAi7beckgpGUC7mGJ2cR1AVPxhYfwVCUX raydium_lock.so
solana program dump -ud metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s mpl_token_metadata.so
solana account -ud 5MxLgy9oPdTC3YgkiePHqr3EoCRD9uLVYRQS2ANAs7wy --output json -o amm_config_0.json
solana account -ud 3oE58BKVt8KuYkGxx8zBojugnymWmBiyafWgMrnb6eYy --output json -o create_pool_fee.json
