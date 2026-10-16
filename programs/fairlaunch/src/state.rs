use anchor_lang::prelude::*;

pub const CONFIG_SEED: &[u8] = b"config";
pub const LAUNCH_SEED: &[u8] = b"launch";
pub const MINT_SEED: &[u8] = b"mint";
pub const COMMIT_SEED: &[u8] = b"commit";
pub const ESCROW_SEED: &[u8] = b"escrow";
pub const GRAD_SEED: &[u8] = b"grad";

pub const DECIMALS: u8 = 6;
/// 1,000,000,000 tokens at 6 decimals.
pub const TOTAL_SUPPLY: u64 = 1_000_000_000_000_000;
/// Tokens the curve can sell.
pub const CURVE_REAL_TOKENS: u64 = 793_100_000_000_000;
/// Tokens reserved for the Raydium pool at graduation.
pub const LP_RESERVE_TOKENS: u64 = TOTAL_SUPPLY - CURVE_REAL_TOKENS;
pub const INITIAL_VIRTUAL_TOKENS: u64 = 1_073_000_000_000_000;
pub const INITIAL_VIRTUAL_SOL: u64 = 30_000_000_000;

/// Smallest pool we will create at graduation.
pub const MIN_POOL_SOL: u64 = 100_000_000;
/// Lamports set aside at graduation for Raydium's pool-creation fee and the
/// rent of the accounts it creates. Whatever is left goes to the treasury.
pub const GRAD_OVERHEAD: u64 = 300_000_000;
/// Lamports the graduation authority keeps so it can re-create its fee
/// accounts when collecting Raydium creator fees.
pub const GRAD_AUTH_FLOAT: u64 = 10_000_000;

/// Hold-streak multiplier ceiling. A payout can never exceed this many times the
/// recipient's share of circulating supply in that epoch's budget.
pub const MAX_STREAK_MULT: u128 = 4;
/// Batches of one reward epoch must land within this window after it starts.
pub const EPOCH_BATCH_WINDOW_SECS: i64 = 900;
/// Hard bounds the config can never leave (even through the timelock).
pub const MAX_BASE_FEE_BPS: u16 = 200;
pub const MAX_SNIPE_FEE_BPS: u16 = 5_000;
pub const MAX_GRADUATION_FEE: u64 = 2_000_000_000;
pub const MAX_DECAY_SLOTS: u64 = 3_000;
pub const MAX_AUCTION_SLOTS: u64 = 3_000;
pub const MIN_REWARD_EPOCH_SECS: i64 = 60;
pub const MAX_TIMELOCK_SECS: i64 = 30 * 86_400;

/// While a coin is on the bonding curve, a curve buy can't take a wallet above this
/// share of the supply (3%). Line commits and the creator's opening buy are held to the same
/// cap when they are made; transfers and Raydium trades aren't limited.
pub const MAX_WALLET_BPS_BONDING: u64 = 300;
pub const MAX_WALLET_TOKENS_BONDING: u64 = TOTAL_SUPPLY / 10_000 * MAX_WALLET_BPS_BONDING;
/// Share of Velvet's cut of Raydium pool fees that goes back into the pool as
/// liquidity (paired with the collected coin side; the LP tokens are burned).
pub const RECYCLE_TREASURY_BPS: u64 = 2_500;

/// Free launches: the creator signs an approval message off-chain and whoever
/// deploys the coin (normally its first buyer) pays the account rent.
/// The message is human-readable text that starts with APPROVAL_PREFIX and ends
/// with APPROVAL_TAG followed by the 64-hex-char digest of the launch parameters.
pub const APPROVAL_PREFIX: &[u8] = b"Velvet launch approval\n";
pub const APPROVAL_TAG: &[u8] = b"\nApproval: ";
pub const APPROVAL_DOMAIN: &[u8] = b"velvet-launch-v1";
pub const MAX_APPROVAL_LEN: usize = 640;
/// Longest an approval may stay valid.
pub const MAX_APPROVAL_SECS: i64 = 30 * 86_400;

pub const MAX_NAME_LEN: usize = 32;
pub const MAX_SYMBOL_LEN: usize = 10;
pub const MAX_URI_LEN: usize = 200;
pub const SEED_LEN: usize = 16;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, InitSpace, PartialEq, Eq, Debug, Default)]
pub struct ConfigParams {
    pub treasury: Pubkey,
    pub distributor: Pubkey,
    pub cpmm_program: Pubkey,
    pub amm_config: Pubkey,
    pub lock_program: Pubkey,
    pub base_fee_bps: u16,
    pub holder_fee_bps: u16,
    pub max_snipe_fee_bps: u16,
    pub cover_to_holders_bps: u16,
    pub pool_fee_creator_bps: u16,
    pub pool_fee_holders_bps: u16,
    pub decay_slots: u64,
    pub min_auction_slots: u64,
    pub max_auction_slots: u64,
    pub per_wallet_commit_cap: u64,
    pub max_creator_buy: u64,
    pub max_min_raise: u64,
    pub max_vest_secs: i64,
    pub graduation_fee: u64,
    pub virtual_sol: u64,
    pub virtual_tokens: u64,
    pub curve_tokens: u64,
    /// Shortest gap between two hold-streak epochs.
    pub min_reward_epoch_secs: i64,
}

#[account]
#[derive(InitSpace)]
pub struct Config {
    pub authority: Pubkey,
    pub treasury: Pubkey,
    /// Key allowed to push hold-streak payouts from a launch's reward pool.
    pub distributor: Pubkey,
    /// Raydium CPMM program and AMM config used for graduation.
    pub cpmm_program: Pubkey,
    pub amm_config: Pubkey,
    /// Raydium liquidity locker ("Burn & Earn").
    pub lock_program: Pubkey,
    /// Normal fee on every curve trade, e.g. 100 = 1%.
    pub base_fee_bps: u16,
    /// Part of the base fee that goes to the holder reward pool, e.g. 25 = 0.25% of volume.
    pub holder_fee_bps: u16,
    /// Fee when the doors open, e.g. 5000 = 50%.
    pub max_snipe_fee_bps: u16,
    /// Share of the cover charge premium (fee above base) sent to holders, in bps of the premium.
    pub cover_to_holders_bps: u16,
    /// Split of Raydium creator fees after graduation, bps of the SOL collected.
    pub pool_fee_creator_bps: u16,
    pub pool_fee_holders_bps: u16,
    pub decay_slots: u64,
    pub min_auction_slots: u64,
    pub max_auction_slots: u64,
    pub per_wallet_commit_cap: u64,
    pub max_creator_buy: u64,
    pub max_min_raise: u64,
    pub max_vest_secs: i64,
    pub graduation_fee: u64,
    /// Curve shape for new launches (mainnet uses the INITIAL_* / CURVE_REAL_TOKENS defaults).
    pub virtual_sol: u64,
    pub virtual_tokens: u64,
    pub curve_tokens: u64,
    pub bump: u8,
    pub min_reward_epoch_secs: i64,
    /// Every config change waits this long between proposal and apply.
    pub timelock_secs: i64,
    /// Queued change (pending_eta == 0 means none).
    pub pending: ConfigParams,
    pub pending_timelock_secs: i64,
    pub pending_eta: i64,
    /// Two-step authority hand-over (e.g. to a multisig).
    pub pending_authority: Pubkey,
    pub _reserved: [u8; 64],
}

#[account]
#[derive(InitSpace)]
pub struct Launch {
    pub creator: Pubkey,
    pub mint: Pubkey,
    pub seed: [u8; SEED_LEN],

    #[max_len(MAX_NAME_LEN)]
    pub name: String,
    #[max_len(MAX_SYMBOL_LEN)]
    pub symbol: String,
    #[max_len(MAX_URI_LEN)]
    pub uri: String,

    // curve
    pub virtual_sol: u64,
    pub virtual_token: u64,
    pub real_sol: u64,
    pub real_token: u64,
    /// Tokens reserved for the Raydium pool (total supply minus curve tokens).
    pub lp_reserve: u64,

    // creator buy, held in escrow and vested
    pub creator_buy_sol: u64,
    pub creator_tokens: u64,
    pub creator_claimed: u64,
    pub vest_secs: i64,
    pub vest_start: i64,

    // opening line
    pub created_slot: u64,
    pub created_at: i64,
    pub auction_end_slot: u64,
    pub min_raise: u64,
    pub total_committed: u64,
    pub committers: u32,
    pub batch_tokens: u64,
    pub batch_refund: u64,
    /// Batch tokens not yet claimed (still in the vault, not part of the LP reserve).
    pub unclaimed_batch: u64,
    /// Lamports held for line refunds and overfill refunds.
    pub escrow_sol: u64,
    pub settled: bool,
    /// Line closed below its minimum: everyone is refunded, the coin never trades.
    pub cancelled: bool,

    // trading
    pub trading_start_slot: u64,
    pub complete: bool,

    // holder rewards (hold streak)
    pub reward_sol: u64,
    pub reward_paid: u64,
    pub reward_epochs: u32,

    // graduation
    /// SOL already moved to the graduation authority, waiting for the pool.
    pub grad_pool_sol: u64,
    pub grad_fee_paid: u64,
    pub grad_prepared: bool,
    pub graduated: bool,
    pub lp_locked: bool,
    /// Fee-key NFT from the locker; held by the graduation authority.
    pub fee_nft_mint: Pubkey,
    pub pool: Pubkey,
    pub graduated_slot: u64,
    pub pool_fees_collected: u64,

    pub bump: u8,
    pub mint_bump: u8,
    pub grad_bump: u8,

    // reward epoch guard (limits what the distributor key can pay out)
    pub epoch_started_at: i64,
    pub epoch_budget: u64,
    pub epoch_paid: u64,
    pub _reserved: [u8; 64],
}

impl Launch {
    /// Lamports the program owes out of this account (rent excluded).
    pub fn liabilities(&self) -> u64 {
        self.real_sol.saturating_add(self.escrow_sol).saturating_add(self.reward_sol)
    }
}

#[account]
#[derive(InitSpace)]
pub struct Commitment {
    pub launch: Pubkey,
    pub owner: Pubkey,
    pub amount: u64,
    pub bump: u8,
}

// ── events ──

#[event]
pub struct LaunchCreated {
    pub launch: Pubkey,
    pub mint: Pubkey,
    pub creator: Pubkey,
    pub creator_buy_sol: u64,
    pub creator_tokens: u64,
    pub auction_end_slot: u64,
    pub min_raise: u64,
    pub vest_secs: i64,
}

#[event]
pub struct Committed {
    pub launch: Pubkey,
    pub user: Pubkey,
    pub amount: u64,
    pub user_total: u64,
    pub launch_total: u64,
}

#[event]
pub struct AuctionSettled {
    pub launch: Pubkey,
    pub total_committed: u64,
    pub batch_tokens: u64,
    pub batch_refund: u64,
    pub fee: u64,
    pub trading_start_slot: u64,
    pub cancelled: bool,
}

#[event]
pub struct Claimed {
    pub launch: Pubkey,
    pub user: Pubkey,
    pub tokens: u64,
    pub refund: u64,
}

#[event]
pub struct Trade {
    pub launch: Pubkey,
    pub user: Pubkey,
    pub is_buy: bool,
    pub sol_amount: u64,
    pub token_amount: u64,
    pub fee: u64,
    pub fee_bps: u16,
    pub to_holders: u64,
    pub virtual_sol: u64,
    pub virtual_token: u64,
}

#[event]
pub struct CreatorVested {
    pub launch: Pubkey,
    pub amount: u64,
    pub total_claimed: u64,
}

#[event]
pub struct CreatorRefunded {
    pub launch: Pubkey,
    pub sol: u64,
    pub tokens_returned: u64,
}

#[event]
pub struct RewardsPaid {
    pub launch: Pubkey,
    pub epoch: u32,
    pub recipients: u32,
    pub total: u64,
    pub remaining: u64,
}

#[event]
pub struct ConfigProposed {
    pub eta: i64,
    pub params: ConfigParams,
    pub timelock_secs: i64,
}

#[event]
pub struct ConfigApplied {
    pub params: ConfigParams,
    pub timelock_secs: i64,
}

#[event]
pub struct AuthorityChanged {
    pub authority: Pubkey,
}

#[event]
pub struct Graduated {
    pub launch: Pubkey,
    pub pool: Pubkey,
    pub sol_to_pool: u64,
    pub tokens_to_pool: u64,
    pub graduation_fee: u64,
}

#[event]
pub struct LiquidityLocked {
    pub launch: Pubkey,
    pub pool: Pubkey,
    pub lp_amount: u64,
    pub fee_nft_mint: Pubkey,
}

#[event]
pub struct LaunchSponsored {
    pub launch: Pubkey,
    pub creator: Pubkey,
    pub payer: Pubkey,
}

#[event]
pub struct LiquidityRecycled {
    pub launch: Pubkey,
    pub sol: u64,
    pub tokens: u64,
    pub lp_burned: u64,
}

#[event]
pub struct PoolFeesCollected {
    pub launch: Pubkey,
    pub sol: u64,
    pub to_treasury: u64,
    pub to_creator: u64,
    pub to_holders: u64,
    pub tokens_burned: u64,
}
