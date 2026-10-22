use anchor_lang::prelude::*;

pub const CONFIG_SEED: &[u8] = b"config";
pub const LAUNCH_SEED: &[u8] = b"launch";
pub const MINT_SEED: &[u8] = b"mint";
/// Every Velvet coin's mint address ends in this (base58), unless ExtParams.allow_any_mint.
pub const VANITY_SUFFIX: &[u8] = b"vLvt";
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
/// Lamports kept back from a Velvet pool's liquidity to pay the rent of the pool and its
/// vaults (about 0.007 SOL); the rest of GRAD_OVERHEAD goes into the pool.
pub const VELVET_SETUP_RESERVE: u64 = 15_000_000;
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
/// Hard ceiling on the per-coin storage fee (0.001 SOL), so changing it needs no timelock.
pub const MAX_STORAGE_FEE: u64 = 1_000_000;

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
    /// Permanent storage: every new coin pays `storage_fee` lamports to `storage_wallet`, which
    /// buys the Arweave credits that store its logo and metadata. Default key = off.
    /// (Carved out of the old 64 reserved bytes, so existing configs read as "off".)
    pub storage_wallet: Pubkey,
    pub storage_fee: u64,
    pub _reserved: [u8; 24],
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
    /// Line unlock schedule, fixed at settle (0 secs = everything claimable at once). Big lines
    /// (see ConfigExt) unlock `line_unlock_bps` at open and the rest linearly over
    /// `line_vest_secs` from `vest_start`. Carved out of the old 64 reserved bytes, so launches
    /// settled before read as "no vesting".
    pub line_unlock_bps: u16,
    pub line_vest_secs: u32,
    /// Where the coin bonded: 0 = Raydium CPMM, 1 = Velvet Pools.
    pub pool_kind: u8,
    /// Highest wallet paid so far in the current payout epoch. Within an epoch every payout
    /// must go to a strictly higher wallet address, so no wallet can be paid twice in one epoch
    /// (across all of its batches). Reset when a new epoch starts.
    pub epoch_last_wallet: Pubkey,
    pub _reserved: [u8; 25],
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
    /// Line tokens already claimed (claims can be partial while a big line unlocks).
    pub claimed: u64,
    /// The overfill refund has been paid.
    pub refunded: bool,
}

/// Size of a commitment created before `claimed`/`refunded` existed (they read as 0/false).
pub const COMMITMENT_V1_LEN: usize = 8 + 32 + 32 + 8 + 1;

/// Velvet Pass: a wallet's record across every Velvet coin, kept by the program.
/// Tiers: 0 Guest, 1 Regular (3+ lines), 2 VIP (10+ lines and 5+ SOL put in, no recent flip).
/// A line counts once, when its tokens are first claimed, if the wallet put in at least
/// PASS_MIN_LINE_SOL and the launch went ahead. A "flip" is a sell while the cover charge is still
/// on: it drops the wallet one tier until it has PASS_HEAL_LINES more clean lines.
#[account]
#[derive(InitSpace)]
pub struct Pass {
    pub owner: Pubkey,
    pub lines: u32,
    pub sol_in_lines: u64,
    pub flips: u32,
    pub flip_pending: bool,
    pub clean_since_flip: u16,
    pub bump: u8,
    pub _reserved: [u8; 32],
}

pub const PASS_SEED: &[u8] = b"pass";
pub const LEDGER_SEED: &[u8] = b"ledger";

/// What one wallet has taken from one coin while it bonds: curve buys in, curve sells out. With the
/// wallet's line share and (for the creator) the opening bag, this is what the 3% cap counts, so
/// moving tokens to another account between buys doesn't make room for more.
#[account]
#[derive(InitSpace)]
pub struct Ledger {
    pub bought: u64,
    pub sold: u64,
    pub bump: u8,
}
pub const PASS_MIN_LINE_SOL: u64 = 100_000_000;
/// Defaults for the "real launch" test a line must pass to count (see ExtParams).
pub const PASS_DEFAULT_MIN_COMMITTERS: u16 = 10;
pub const PASS_DEFAULT_MIN_LINE_TOTAL: u64 = 10_000_000_000;
pub const PASS_REGULAR_LINES: u32 = 3;
pub const PASS_VIP_LINES: u32 = 10;
pub const PASS_VIP_SOL: u64 = 5_000_000_000;
pub const PASS_HEAL_LINES: u16 = 5;
/// VIPs pay this share of the cover charge premium (the part above the base fee), in bps.
pub const PASS_VIP_PREMIUM_BPS: u64 = 5_000;

impl Pass {
    pub fn tier(&self) -> u8 {
        let base = if self.lines >= PASS_VIP_LINES && self.sol_in_lines >= PASS_VIP_SOL { 2 } else if self.lines >= PASS_REGULAR_LINES { 1 } else { 0 };
        if self.flip_pending && base > 0 { base - 1 } else { base }
    }
}

#[event]
pub struct PassUpdated {
    pub owner: Pubkey,
    pub lines: u32,
    pub sol_in_lines: u64,
    pub flips: u32,
    pub tier: u8,
}

/// Extra settings that didn't fit the original Config, behind the same timelock.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, InitSpace, PartialEq, Eq, Debug, Default)]
pub struct ExtParams {
    /// A line that puts more than this into the curve unlocks gradually.
    pub big_line_sol: u64,
    /// Share of each big-line spot claimable when the doors open (bps).
    pub big_line_unlock_bps: u16,
    /// The rest unlocks linearly over this long.
    pub big_line_vest_secs: u32,
    /// New coins bond into Velvet Pools instead of Raydium.
    pub velvet_pools: bool,
    /// Velvet Pass: a line only counts in a launch with at least this many wallets in its line
    /// (0 = PASS_DEFAULT_MIN_COMMITTERS).
    pub pass_min_committers: u16,
    /// ...and at least this much SOL in its line in total (0 = PASS_DEFAULT_MIN_LINE_TOTAL).
    pub pass_min_line_total: u64,
    /// Let coins launch at any mint address. Off (the default), every coin's mint must end in
    /// VANITY_SUFFIX.
    pub allow_any_mint: bool,
    /// Room for later settings without moving the layout.
    pub _spare: [u8; 20],
    pub _spare2: [u8; 32],
}

#[account]
#[derive(InitSpace)]
pub struct ConfigExt {
    pub bump: u8,
    pub params: ExtParams,
    pub pending: ExtParams,
    pub pending_eta: i64,
    pub _reserved: [u8; 128],
}

pub const CONFIG_EXT_SEED: &[u8] = b"config_ext";
pub const MAX_LINE_VEST_SECS: u32 = 7 * 86_400;
pub const MIN_LINE_UNLOCK_BPS: u16 = 1_000;
/// When the line would fill the whole curve while the cover charge is on, it stops this many
/// tokens short so the curve can only complete (and bond) after the cover charge.
pub const SETTLE_SLIVER_TOKENS: u64 = 1_000_000;

// ── events ──

#[event]
pub struct ConfigExtProposed {
    pub eta: i64,
    pub params: ExtParams,
}

#[event]
pub struct ConfigExtApplied {
    pub params: ExtParams,
}

#[event]
pub struct StorageSet {
    pub wallet: Pubkey,
    pub fee: u64,
}

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
