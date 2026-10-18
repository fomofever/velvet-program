//! FairLaunch v2 — the program behind Velvet.
//!
//! Anti-snipe launches:
//!   1. The program deploys every coin. The mint is a PDA (Velvet grinds seeds so
//!      addresses end in "vLvt"), metadata is written, minting is switched off.
//!   2. The creator's opening buy happens in the launch transaction, at the start
//!      price, into an escrow that vests linearly.
//!   3. The curve is locked for an opening window. Buyers commit SOL ("the line").
//!   4. Settlement runs the whole line as one buy at one price. If the line is below
//!      the creator's minimum, the launch is cancelled and everyone is refunded.
//!   5. Trading opens with a fee that decays from 50% to 1%. Part of every fee, and
//!      most of the cover-charge premium, goes to a holder reward pool paid out by
//!      hold streak.
//!   6. While bonding, no curve buy can take a wallet above 3% of the supply, and no
//!      wallet's line commit (or the creator's opening buy) can buy more than 3% either.
//!   7. When the curve fills, anyone can graduate it into a Raydium CPMM pool. All LP
//!      is locked in Raydium's locker; its trading fees go to the treasury, the
//!      creator and holders, and a quarter of the treasury's part goes back into the
//!      pool as liquidity whose LP tokens are burned.
//!   8. Free launches: the creator only signs an approval message; whoever deploys
//!      the coin (normally its first buyer) pays the account rent.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program::invoke_signed;
use anchor_lang::system_program;
use anchor_spl::associated_token::{self, AssociatedToken};
use anchor_spl::metadata::{
    create_metadata_accounts_v3, mpl_token_metadata::types::DataV2, CreateMetadataAccountsV3, Metadata,
};
use anchor_spl::token::{self, spl_token, Mint, SetAuthority, Token, TokenAccount, Transfer};

pub mod errors;
pub mod math;
pub mod raydium;
pub mod state;

use errors::{FairError, MathExt};
use state::*;

declare_id!("8f9XtMzn9J8vapxoLoypKUjNta64jhjfr18WMVCn5yAk");

impl ConfigParams {
    fn validate(&self) -> Result<()> {
        require!(
            self.base_fee_bps <= MAX_BASE_FEE_BPS
                && self.holder_fee_bps <= self.base_fee_bps
                && self.max_snipe_fee_bps <= MAX_SNIPE_FEE_BPS
                && self.base_fee_bps <= self.max_snipe_fee_bps
                && self.graduation_fee <= MAX_GRADUATION_FEE
                && self.decay_slots <= MAX_DECAY_SLOTS
                && self.max_auction_slots <= MAX_AUCTION_SLOTS
                && self.min_reward_epoch_secs >= MIN_REWARD_EPOCH_SECS
                && self.treasury != Pubkey::default()
                && self.cover_to_holders_bps <= 10_000
                && (self.pool_fee_creator_bps as u32 + self.pool_fee_holders_bps as u32) <= 10_000
                && self.min_auction_slots > 0
                && self.min_auction_slots <= self.max_auction_slots
                && self.per_wallet_commit_cap > 0
                && self.max_vest_secs >= 0
                && self.virtual_sol > 0
                && self.curve_tokens > 0
                && self.curve_tokens < TOTAL_SUPPLY
                && self.virtual_tokens > self.curve_tokens,
            FairError::InvalidConfig
        );
        Ok(())
    }
    fn apply(&self, c: &mut Config) {
        c.treasury = self.treasury;
        c.distributor = self.distributor;
        c.cpmm_program = self.cpmm_program;
        c.amm_config = self.amm_config;
        c.lock_program = self.lock_program;
        c.base_fee_bps = self.base_fee_bps;
        c.holder_fee_bps = self.holder_fee_bps;
        c.max_snipe_fee_bps = self.max_snipe_fee_bps;
        c.cover_to_holders_bps = self.cover_to_holders_bps;
        c.pool_fee_creator_bps = self.pool_fee_creator_bps;
        c.pool_fee_holders_bps = self.pool_fee_holders_bps;
        c.decay_slots = self.decay_slots;
        c.min_auction_slots = self.min_auction_slots;
        c.max_auction_slots = self.max_auction_slots;
        c.per_wallet_commit_cap = self.per_wallet_commit_cap;
        c.max_creator_buy = self.max_creator_buy;
        c.max_min_raise = self.max_min_raise;
        c.max_vest_secs = self.max_vest_secs;
        c.graduation_fee = self.graduation_fee;
        c.virtual_sol = self.virtual_sol;
        c.virtual_tokens = self.virtual_tokens;
        c.curve_tokens = self.curve_tokens;
        c.min_reward_epoch_secs = self.min_reward_epoch_secs;
    }
}

// ───────────────────────────── helpers ─────────────────────────────

/// Move lamports out of an account this program owns.
fn pay_from_pda<'info>(from: &AccountInfo<'info>, to: &AccountInfo<'info>, amount: u64) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    **from.try_borrow_mut_lamports()? = from.lamports().checked_sub(amount).ok_or(FairError::Overflow)?;
    **to.try_borrow_mut_lamports()? = to.lamports().checked_add(amount).ok_or(FairError::Overflow)?;
    Ok(())
}

fn pay_from_user<'info>(system: &AccountInfo<'info>, from: &AccountInfo<'info>, to: &AccountInfo<'info>, amount: u64) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    system_program::transfer(
        CpiContext::new(system.clone(), system_program::Transfer { from: from.clone(), to: to.clone() }),
        amount,
    )
}

/// System transfer out of a PDA that is a plain system account (the graduation authority).
fn pay_from_system_pda<'info>(system: &AccountInfo<'info>, from: &AccountInfo<'info>, to: &AccountInfo<'info>, amount: u64, seeds: &[&[&[u8]]]) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    system_program::transfer(
        CpiContext::new_with_signer(system.clone(), system_program::Transfer { from: from.clone(), to: to.clone() }, seeds),
        amount,
    )
}

/// Split a fee: holders' part stays in the launch account (reward pool), the rest
/// goes to the treasury. Returns the holders' part. `fee_source_is_launch` says
/// whether the fee lamports already sit in the launch account.
fn route_fee<'info>(
    cfg: &Config,
    l: &mut Account<'info, Launch>,
    fee: u64,
    fee_bps: u16,
    payer: Option<(&AccountInfo<'info>, &AccountInfo<'info>)>, // (system_program, user) if a user pays
    treasury: &AccountInfo<'info>,
) -> Result<u64> {
    let (to_holders, to_treasury) =
        math::split_fee(fee, fee_bps, cfg.base_fee_bps, cfg.holder_fee_bps, cfg.cover_to_holders_bps).fx()?;
    match payer {
        Some((system, user)) => {
            pay_from_user(system, user, &l.to_account_info(), to_holders)?;
            pay_from_user(system, user, treasury, to_treasury)?;
        }
        None => {
            pay_from_pda(&l.to_account_info(), treasury, to_treasury)?;
        }
    }
    l.reward_sol = l.reward_sol.checked_add(to_holders).ok_or(FairError::Overflow)?;
    Ok(to_holders)
}

macro_rules! launch_signer {
    ($launch:expr, $name:ident) => {
        let __mint: Pubkey = $launch.mint;
        let __bump = [$launch.bump];
        let __seeds: [&[u8]; 3] = [LAUNCH_SEED, __mint.as_ref(), &__bump];
        let $name: &[&[&[u8]]] = &[&__seeds];
    };
}

macro_rules! grad_signer {
    ($launch_key:expr, $bump:expr, $name:ident) => {
        let __key: Pubkey = $launch_key;
        let __bump = [$bump];
        let __seeds: [&[u8]; 3] = [GRAD_SEED, __key.as_ref(), &__bump];
        let $name: &[&[&[u8]]] = &[&__seeds];
    };
}

fn fee_now(cfg: &Config, l: &Launch, slot: u64) -> u16 {
    math::fee_bps_at(slot, l.trading_start_slot, cfg.decay_slots, cfg.max_snipe_fee_bps, cfg.base_fee_bps)
}

fn token_amount(acc: &AccountInfo) -> Result<u64> {
    let data = acc.try_borrow_data()?;
    let a = spl_token::state::Account::unpack_from_slice(&data).map_err(|_| error!(FairError::WrongPoolAccount))?;
    Ok(a.amount)
}

use anchor_lang::solana_program::program_pack::Pack;

/// Read a commitment of either layout (before/after `claimed` and `refunded` were added).
fn read_commitment(info: &AccountInfo) -> Result<Commitment> {
    require!(info.owner == &crate::ID, FairError::BadCommitment);
    let data = info.try_borrow_data()?;
    require!(data.len() >= COMMITMENT_V1_LEN && data[..8] == *Commitment::DISCRIMINATOR, FairError::BadCommitment);
    let mut b = &data[8..];
    let launch = Pubkey::deserialize(&mut b)?;
    let owner = Pubkey::deserialize(&mut b)?;
    let amount = u64::deserialize(&mut b)?;
    let bump = u8::deserialize(&mut b)?;
    let (claimed, refunded) = if data.len() >= 8 + Commitment::INIT_SPACE { (u64::deserialize(&mut b)?, bool::deserialize(&mut b)?) } else { (0, false) };
    Ok(Commitment { launch, owner, amount, bump, claimed, refunded })
}

/// Write a commitment back in the current layout, growing an old one first (the user pays the
/// few lamports of extra rent).
fn write_commitment<'info>(info: &AccountInfo<'info>, c: &Commitment, payer: &AccountInfo<'info>, system: &AccountInfo<'info>) -> Result<()> {
    let need = 8 + Commitment::INIT_SPACE;
    if info.data_len() < need {
        let rent = Rent::get()?.minimum_balance(need).saturating_sub(info.lamports());
        if rent > 0 {
            anchor_lang::system_program::transfer(
                CpiContext::new(system.clone(), anchor_lang::system_program::Transfer { from: payer.clone(), to: info.clone() }),
                rent,
            )?;
        }
        info.realloc(need, false)?;
    }
    let mut data = info.try_borrow_mut_data()?;
    let mut out: &mut [u8] = &mut data[8..];
    c.serialize(&mut out)?;
    Ok(())
}

/// Close a program-owned account, sending its lamports to `to`.
fn close_account<'info>(info: &AccountInfo<'info>, to: &AccountInfo<'info>) -> Result<()> {
    let lamports = info.lamports();
    **to.try_borrow_mut_lamports()? = to.lamports().checked_add(lamports).ok_or(FairError::Overflow)?;
    **info.try_borrow_mut_lamports()? = 0;
    info.assign(&anchor_lang::solana_program::system_program::ID);
    info.realloc(0, false)?;
    Ok(())
}

/// While the cover charge is still above the base fee, nothing may complete the curve: the coin
/// can only bond (and leave the cover charge behind) after it.
fn in_cover_window(cfg: &Config, l: &Launch, slot: u64) -> bool {
    cfg.decay_slots > 0 && slot < l.trading_start_slot.saturating_add(cfg.decay_slots)
}

/// Fill in a freshly created Pass (init_if_needed leaves it zeroed).
fn touch_pass(p: &mut Pass, owner: Pubkey, bump: u8) {
    if p.owner == Pubkey::default() { p.owner = owner; p.bump = bump; }
}

/// The cover charge a wallet pays right now: VIPs pay half of the premium above the base fee.
fn fee_for(cfg: &Config, l: &Launch, slot: u64, pass: &Pass) -> u16 {
    let f = fee_now(cfg, l, slot);
    if pass.tier() >= 2 && f > cfg.base_fee_bps {
        let premium = (f - cfg.base_fee_bps) as u64;
        cfg.base_fee_bps + (premium * PASS_VIP_PREMIUM_BPS / 10_000) as u16
    } else {
        f
    }
}

impl ExtParams {
    pub fn validate(&self) -> Result<()> {
        require!(
            (MIN_LINE_UNLOCK_BPS..=10_000).contains(&self.big_line_unlock_bps) && self.big_line_vest_secs <= MAX_LINE_VEST_SECS,
            FairError::InvalidConfig
        );
        Ok(())
    }
}

/// What a creator's launch approval commits to. Anyone holding a valid signature
/// over this can deploy exactly this coin, and only once (the mint address comes
/// from the seed).
#[allow(clippy::too_many_arguments)]
pub fn approval_digest(
    seed: &[u8; SEED_LEN],
    name: &str,
    symbol: &str,
    uri: &str,
    auction_slots: u64,
    min_raise: u64,
    vest_secs: i64,
    expires_at: i64,
) -> [u8; 32] {
    anchor_lang::solana_program::hash::hashv(&[
        APPROVAL_DOMAIN,
        crate::ID.as_ref(),
        seed,
        name.as_bytes(),
        &[0],
        symbol.as_bytes(),
        &[0],
        uri.as_bytes(),
        &[0],
        &auction_slots.to_le_bytes(),
        &min_raise.to_le_bytes(),
        &vest_secs.to_le_bytes(),
        &expires_at.to_le_bytes(),
    ])
    .to_bytes()
}

fn hex_lower(bytes: &[u8]) -> Vec<u8> {
    const H: &[u8; 16] = b"0123456789abcdef";
    bytes.iter().flat_map(|b| [H[(b >> 4) as usize], H[(b & 15) as usize]]).collect()
}

/// The instruction right before this one must be the Ed25519 program verifying one
/// signature by `creator` over an approval message (all data inline in that
/// instruction) that starts with APPROVAL_PREFIX and ends with APPROVAL_TAG + the
/// hex digest. The Ed25519 program has already checked the signature itself.
fn check_approval(ix_sysvar: &AccountInfo, creator: &Pubkey, digest: &[u8; 32]) -> Result<()> {
    use anchor_lang::solana_program::sysvar::instructions::{load_current_index_checked, load_instruction_at_checked};
    let cur = load_current_index_checked(ix_sysvar)?;
    require!(cur > 0, FairError::BadApproval);
    let ed = load_instruction_at_checked((cur - 1) as usize, ix_sysvar)?;
    require_keys_eq!(ed.program_id, anchor_lang::solana_program::ed25519_program::ID, FairError::BadApproval);
    let d = &ed.data;
    require!(d.len() >= 16 && d[0] == 1, FairError::BadApproval);
    let rd = |i: usize| u16::from_le_bytes([d[i], d[i + 1]]) as usize;
    let (pk_off, msg_off, msg_len) = (rd(6), rd(10), rd(12));
    // Signature, key and message must all sit inside that same instruction.
    require!(rd(4) == 0xFFFF && rd(8) == 0xFFFF && rd(14) == 0xFFFF, FairError::BadApproval);
    let pk = d.get(pk_off..pk_off + 32).ok_or(FairError::BadApproval)?;
    require!(pk == creator.as_ref(), FairError::BadApproval);
    let msg = d.get(msg_off..msg_off + msg_len).ok_or(FairError::BadApproval)?;
    require!(msg.len() <= MAX_APPROVAL_LEN && msg.starts_with(APPROVAL_PREFIX), FairError::BadApproval);
    let mut tail = APPROVAL_TAG.to_vec();
    tail.extend_from_slice(&hex_lower(digest));
    require!(msg.ends_with(&tail), FairError::BadApproval);
    Ok(())
}

/// Fill in a new launch and mint the coin: metadata (immutable, update authority is
/// the launch PDA), the fixed supply into the vault, minting switched off.
#[allow(clippy::too_many_arguments)]
fn init_coin<'info>(
    cfg: &Config,
    l: &mut Account<'info, Launch>,
    creator: Pubkey,
    a: CoinAccounts<'_, 'info>,
    seed: [u8; SEED_LEN],
    name: String,
    symbol: String,
    uri: String,
    auction_slots: u64,
    min_raise: u64,
    vest_secs: i64,
    bump: u8,
    mint_bump: u8,
) -> Result<()> {
    require!(name.len() <= MAX_NAME_LEN && symbol.len() <= MAX_SYMBOL_LEN && uri.len() <= MAX_URI_LEN, FairError::MetadataTooLong);
    require!(auction_slots >= cfg.min_auction_slots && auction_slots <= cfg.max_auction_slots, FairError::InvalidAuctionWindow);
    require!(min_raise <= cfg.max_min_raise, FairError::MinRaiseTooHigh);
    require!(vest_secs >= 0 && vest_secs <= cfg.max_vest_secs, FairError::VestTooLong);

    let clock = Clock::get()?;
    l.creator = creator;
    l.mint = a.mint.key();
    l.seed = seed;
    l.name = name.clone();
    l.symbol = symbol.clone();
    l.uri = uri.clone();
    l.virtual_sol = cfg.virtual_sol;
    l.virtual_token = cfg.virtual_tokens;
    l.real_token = cfg.curve_tokens;
    l.lp_reserve = TOTAL_SUPPLY - cfg.curve_tokens;
    l.created_slot = clock.slot;
    l.created_at = clock.unix_timestamp;
    l.auction_end_slot = clock.slot.checked_add(auction_slots).ok_or(FairError::Overflow)?;
    l.min_raise = min_raise;
    l.vest_secs = vest_secs;
    l.bump = bump;
    l.mint_bump = mint_bump;
    l.grad_bump = Pubkey::find_program_address(&[GRAD_SEED, l.key().as_ref()], &crate::ID).1;

    launch_signer!(l, signer);
    create_metadata_accounts_v3(
        CpiContext::new_with_signer(
            a.metadata_program.clone(),
            CreateMetadataAccountsV3 {
                metadata: a.metadata.clone(),
                mint: a.mint.clone(),
                mint_authority: l.to_account_info(),
                payer: a.payer.clone(),
                update_authority: l.to_account_info(),
                system_program: a.system.clone(),
                rent: a.rent.clone(),
            },
            signer,
        ),
        DataV2 { name, symbol, uri, seller_fee_basis_points: 0, creators: None, collection: None, uses: None },
        false,
        true,
        None,
    )?;
    token::mint_to(
        CpiContext::new_with_signer(
            a.token_program.clone(),
            token::MintTo { mint: a.mint.clone(), to: a.vault.clone(), authority: l.to_account_info() },
            signer,
        ),
        TOTAL_SUPPLY,
    )?;
    token::set_authority(
        CpiContext::new_with_signer(
            a.token_program.clone(),
            SetAuthority { current_authority: l.to_account_info(), account_or_mint: a.mint.clone() },
            signer,
        ),
        spl_token::instruction::AuthorityType::MintTokens,
        None,
    )?;
    Ok(())
}

struct CoinAccounts<'a, 'info> {
    mint: &'a AccountInfo<'info>,
    vault: &'a AccountInfo<'info>,
    metadata: &'a AccountInfo<'info>,
    metadata_program: &'a AccountInfo<'info>,
    payer: &'a AccountInfo<'info>,
    system: &'a AccountInfo<'info>,
    rent: &'a AccountInfo<'info>,
    token_program: &'a AccountInfo<'info>,
}

/// The per-coin storage fee, paid by whoever pays the coin's rent. Skipped when storage is off,
/// or when the wallet is so empty that the transfer itself would fail Solana's rent rule
/// (a launch must never fail over this).
fn pay_storage_fee<'info>(cfg: &Config, storage: &AccountInfo<'info>, payer: &AccountInfo<'info>, system: &AccountInfo<'info>) -> Result<()> {
    if cfg.storage_wallet == Pubkey::default() || cfg.storage_fee == 0 { return Ok(()); }
    require_keys_eq!(storage.key(), cfg.storage_wallet, FairError::WrongStorageWallet);
    let min = Rent::get()?.minimum_balance(0);
    if storage.lamports().saturating_add(cfg.storage_fee) < min { return Ok(()); }
    anchor_lang::system_program::transfer(
        CpiContext::new(system.clone(), anchor_lang::system_program::Transfer { from: payer.clone(), to: storage.clone() }),
        cfg.storage_fee,
    )
}

#[program]
pub mod fairlaunch {
    use super::*;

    pub fn init_config(ctx: Context<InitConfig>, params: ConfigParams, timelock_secs: i64) -> Result<()> {
        params.validate()?;
        require!((0..=MAX_TIMELOCK_SECS).contains(&timelock_secs), FairError::InvalidConfig);
        let c = &mut ctx.accounts.config;
        c.authority = ctx.accounts.authority.key();
        c.bump = ctx.bumps.config;
        c.timelock_secs = timelock_secs;
        params.apply(c);
        Ok(())
    }

    /// Queue a config change. It can only be applied after the current timelock,
    /// so everyone sees fee or treasury changes coming. The Raydium program, AMM
    /// config and locker are fixed for the life of the program.
    pub fn propose_config(ctx: Context<AdminConfig>, params: ConfigParams, timelock_secs: i64) -> Result<()> {
        params.validate()?;
        require!((0..=MAX_TIMELOCK_SECS).contains(&timelock_secs), FairError::InvalidConfig);
        let c = &mut ctx.accounts.config;
        require!(
            params.cpmm_program == c.cpmm_program && params.amm_config == c.amm_config && params.lock_program == c.lock_program,
            FairError::ImmutableField
        );
        let eta = Clock::get()?.unix_timestamp.checked_add(c.timelock_secs).ok_or(FairError::Overflow)?.max(1);
        c.pending = params.clone();
        c.pending_timelock_secs = timelock_secs;
        c.pending_eta = eta;
        emit!(ConfigProposed { eta, params, timelock_secs });
        Ok(())
    }

    /// Apply the queued change once its timelock has passed. Anyone can call it.
    pub fn apply_config(ctx: Context<ApplyConfig>) -> Result<()> {
        let c = &mut ctx.accounts.config;
        require!(c.pending_eta != 0, FairError::NoPendingChange);
        require!(Clock::get()?.unix_timestamp >= c.pending_eta, FairError::TimelockNotReady);
        let p = c.pending.clone();
        p.validate()?;
        p.apply(c);
        c.timelock_secs = c.pending_timelock_secs;
        c.pending = ConfigParams::default();
        c.pending_eta = 0;
        emit!(ConfigApplied { params: p, timelock_secs: c.timelock_secs });
        Ok(())
    }

    pub fn cancel_config(ctx: Context<AdminConfig>) -> Result<()> {
        let c = &mut ctx.accounts.config;
        c.pending = ConfigParams::default();
        c.pending_eta = 0;
        Ok(())
    }

    /// Emergency brake: instantly switch hold-streak payouts off (no timelock, since
    /// it can only stop money moving). Turning them back on goes through the timelock.
    pub fn disable_distributor(ctx: Context<AdminConfig>) -> Result<()> {
        ctx.accounts.config.distributor = Pubkey::default();
        Ok(())
    }

    /// Hand the config to a new authority (e.g. a Squads multisig). Two steps so a
    /// typo can't lose control.
    /// Set the storage wallet and per-coin fee (default key turns it off). The fee is capped at
    /// MAX_STORAGE_FEE, so this takes effect immediately.
    pub fn set_storage(ctx: Context<AdminConfig>, wallet: Pubkey, fee: u64) -> Result<()> {
        require!(fee <= MAX_STORAGE_FEE, FairError::InvalidConfig);
        let c = &mut ctx.accounts.config;
        c.storage_wallet = wallet;
        c.storage_fee = fee;
        emit!(StorageSet { wallet, fee });
        Ok(())
    }

    /// Create the extra settings account (once). Its values then change only through the timelock.
    pub fn init_config_ext(ctx: Context<InitConfigExt>, params: ExtParams) -> Result<()> {
        params.validate()?;
        let e = &mut ctx.accounts.config_ext;
        e.bump = ctx.bumps.config_ext;
        e.params = params;
        Ok(())
    }

    pub fn propose_config_ext(ctx: Context<AdminConfigExt>, params: ExtParams) -> Result<()> {
        params.validate()?;
        let eta = Clock::get()?.unix_timestamp.checked_add(ctx.accounts.config.timelock_secs).ok_or(FairError::Overflow)?.max(1);
        let e = &mut ctx.accounts.config_ext;
        e.pending = params.clone();
        e.pending_eta = eta;
        emit!(ConfigExtProposed { eta, params });
        Ok(())
    }

    /// Anyone can apply the queued extra settings once the timelock has passed.
    pub fn apply_config_ext(ctx: Context<ApplyConfigExt>) -> Result<()> {
        let e = &mut ctx.accounts.config_ext;
        require!(e.pending_eta != 0, FairError::NoPendingChange);
        require!(Clock::get()?.unix_timestamp >= e.pending_eta, FairError::TimelockNotReady);
        let p = e.pending.clone();
        p.validate()?;
        e.params = p.clone();
        e.pending = ExtParams::default();
        e.pending_eta = 0;
        emit!(ConfigExtApplied { params: p });
        Ok(())
    }

    pub fn cancel_config_ext(ctx: Context<AdminConfigExt>) -> Result<()> {
        let e = &mut ctx.accounts.config_ext;
        e.pending = ExtParams::default();
        e.pending_eta = 0;
        Ok(())
    }

    pub fn transfer_authority(ctx: Context<AdminConfig>, new_authority: Pubkey) -> Result<()> {
        ctx.accounts.config.pending_authority = new_authority;
        Ok(())
    }

    pub fn accept_authority(ctx: Context<AcceptAuthority>) -> Result<()> {
        let c = &mut ctx.accounts.config;
        require_keys_eq!(ctx.accounts.new_authority.key(), c.pending_authority, FairError::NotPendingAuthority);
        c.authority = c.pending_authority;
        c.pending_authority = Pubkey::default();
        emit!(AuthorityChanged { authority: c.authority });
        Ok(())
    }

    /// Create the coin: PDA mint, metadata, fixed supply, minting off, creator buy
    /// into vesting escrow, opening line starts. All in one instruction.
    #[allow(clippy::too_many_arguments)]
    pub fn create_launch(
        ctx: Context<CreateLaunch>,
        seed: [u8; SEED_LEN],
        name: String,
        symbol: String,
        uri: String,
        auction_slots: u64,
        creator_buy_lamports: u64,
        min_raise: u64,
        vest_secs: i64,
    ) -> Result<()> {
        let cfg = &ctx.accounts.config;
        require!(creator_buy_lamports <= cfg.max_creator_buy, FairError::CreatorBuyTooLarge);
        require_keys_eq!(ctx.accounts.treasury.key(), cfg.treasury, FairError::WrongTreasury);
        let (bump, mint_bump) = (ctx.bumps.launch, ctx.bumps.mint);
        let (mint, vault, metadata, metadata_program, creator_info, system, rent, token_prog) = (
            ctx.accounts.mint.to_account_info(),
            ctx.accounts.vault.to_account_info(),
            ctx.accounts.metadata.to_account_info(),
            ctx.accounts.metadata_program.to_account_info(),
            ctx.accounts.creator.to_account_info(),
            ctx.accounts.system_program.to_account_info(),
            ctx.accounts.rent.to_account_info(),
            ctx.accounts.token_program.to_account_info(),
        );
        let creator_key = ctx.accounts.creator.key();
        let l = &mut ctx.accounts.launch;
        init_coin(
            cfg,
            l,
            creator_key,
            CoinAccounts { mint: &mint, vault: &vault, metadata: &metadata, metadata_program: &metadata_program, payer: &creator_info, system: &system, rent: &rent, token_program: &token_prog },
            seed, name, symbol, uri, auction_slots, min_raise, vest_secs, bump, mint_bump,
        )?;
        pay_storage_fee(cfg, &ctx.accounts.storage.to_account_info(), &creator_info, &system)?;
        launch_signer!(l, signer);

        if creator_buy_lamports > 0 {
            let fee = math::fee_amount(creator_buy_lamports, cfg.base_fee_bps).fx()?;
            let q = math::quote_buy(l.virtual_sol, l.virtual_token, l.real_token, creator_buy_lamports - fee).fx()?;
            // The creator's bag counts against the same 3% cap as everyone else.
            require!(q.tokens_out <= MAX_WALLET_TOKENS_BONDING, FairError::WalletCapExceeded);
            let system = ctx.accounts.system_program.to_account_info();
            let creator = ctx.accounts.creator.to_account_info();
            pay_from_user(&system, &creator, &l.to_account_info(), q.sol_used)?;
            route_fee(cfg, l, fee, cfg.base_fee_bps, Some((&system, &creator)), &ctx.accounts.treasury.to_account_info())?;
            token::transfer(
                CpiContext::new_with_signer(
                    token_prog,
                    Transfer { from: ctx.accounts.vault.to_account_info(), to: ctx.accounts.escrow.to_account_info(), authority: l.to_account_info() },
                    signer,
                ),
                q.tokens_out,
            )?;
            l.virtual_sol += q.sol_used;
            l.virtual_token -= q.tokens_out;
            l.real_sol += q.sol_used;
            l.real_token -= q.tokens_out;
            l.creator_buy_sol = q.sol_used;
            l.creator_tokens = q.tokens_out;
        }

        emit!(LaunchCreated {
            launch: l.key(),
            mint: l.mint,
            creator: l.creator,
            creator_buy_sol: l.creator_buy_sol,
            creator_tokens: l.creator_tokens,
            auction_end_slot: l.auction_end_slot,
            min_raise,
            vest_secs,
        });
        Ok(())
    }

    /// Free launch: the creator signed an approval off-chain (an Ed25519
    /// instruction right before this one); whoever sends this transaction pays the
    /// account rent, normally the coin's first buyer joining the line in the same
    /// transaction. The coin is identical to a normal launch without an opening buy.
    #[allow(clippy::too_many_arguments)]
    pub fn create_launch_signed(
        ctx: Context<CreateLaunchSigned>,
        seed: [u8; SEED_LEN],
        name: String,
        symbol: String,
        uri: String,
        auction_slots: u64,
        min_raise: u64,
        vest_secs: i64,
        expires_at: i64,
        creator: Pubkey,
    ) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        require!(now <= expires_at, FairError::ApprovalExpired);
        require!(expires_at <= now.saturating_add(MAX_APPROVAL_SECS), FairError::BadApproval);
        require!(creator != Pubkey::default(), FairError::BadApproval);
        let digest = approval_digest(&seed, &name, &symbol, &uri, auction_slots, min_raise, vest_secs, expires_at);
        check_approval(&ctx.accounts.instructions.to_account_info(), &creator, &digest)?;

        let cfg = &ctx.accounts.config;
        let (bump, mint_bump) = (ctx.bumps.launch, ctx.bumps.mint);
        let (mint, vault, metadata, metadata_program, payer, system, rent, token_prog) = (
            ctx.accounts.mint.to_account_info(),
            ctx.accounts.vault.to_account_info(),
            ctx.accounts.metadata.to_account_info(),
            ctx.accounts.metadata_program.to_account_info(),
            ctx.accounts.payer.to_account_info(),
            ctx.accounts.system_program.to_account_info(),
            ctx.accounts.rent.to_account_info(),
            ctx.accounts.token_program.to_account_info(),
        );
        let payer_key = payer.key();
        let l = &mut ctx.accounts.launch;
        init_coin(
            cfg,
            l,
            creator,
            CoinAccounts { mint: &mint, vault: &vault, metadata: &metadata, metadata_program: &metadata_program, payer: &payer, system: &system, rent: &rent, token_program: &token_prog },
            seed, name, symbol, uri, auction_slots, min_raise, vest_secs, bump, mint_bump,
        )?;
        pay_storage_fee(cfg, &ctx.accounts.storage.to_account_info(), &payer, &system)?;
        emit!(LaunchCreated {
            launch: l.key(),
            mint: l.mint,
            creator,
            creator_buy_sol: 0,
            creator_tokens: 0,
            auction_end_slot: l.auction_end_slot,
            min_raise,
            vest_secs,
        });
        emit!(LaunchSponsored { launch: l.key(), creator, payer: payer_key });
        Ok(())
    }

    /// Commit SOL to the opening line.
    pub fn commit(ctx: Context<Commit>, lamports: u64) -> Result<()> {
        require!(lamports > 0, FairError::ZeroAmount);
        let slot = Clock::get()?.slot;
        let l = &mut ctx.accounts.launch;
        require!(!l.settled && slot < l.auction_end_slot, FairError::AuctionClosed);
        let c = &mut ctx.accounts.commitment;
        if c.amount == 0 {
            c.launch = l.key();
            c.owner = ctx.accounts.user.key();
            c.bump = ctx.bumps.commitment;
            l.committers = l.committers.checked_add(1).ok_or(FairError::Overflow)?;
        }
        let new_amount = c.amount.checked_add(lamports).ok_or(FairError::Overflow)?;
        require!(new_amount <= ctx.accounts.config.per_wallet_commit_cap, FairError::CommitCapExceeded);
        // 3% cap for the line too: what this wallet's whole commit would buy if it were alone in
        // the line, at the price before the line, must fit under the cap (minus the creator's own
        // bag, for the creator). A fuller line only raises the price, so the real share is smaller.
        {
            let fee = math::fee_amount(new_amount, ctx.accounts.config.base_fee_bps).fx()?;
            let q = math::quote_buy(l.virtual_sol, l.virtual_token, l.real_token, new_amount - fee).fx()?;
            let own = if c.owner == l.creator { l.creator_tokens } else { 0 };
            require!(q.tokens_out.saturating_add(own) <= MAX_WALLET_TOKENS_BONDING, FairError::WalletCapExceeded);
        }
        pay_from_user(&ctx.accounts.system_program.to_account_info(), &ctx.accounts.user.to_account_info(), &l.to_account_info(), lamports)?;
        c.amount = new_amount;
        l.total_committed = l.total_committed.checked_add(lamports).ok_or(FairError::Overflow)?;
        l.escrow_sol = l.escrow_sol.checked_add(lamports).ok_or(FairError::Overflow)?;
        emit!(Committed { launch: l.key(), user: c.owner, amount: lamports, user_total: c.amount, launch_total: l.total_committed });
        Ok(())
    }

    /// Permissionless. After the window: cancel if below the minimum, otherwise run
    /// the whole line as one buy at one price and open trading.
    pub fn settle(ctx: Context<Settle>) -> Result<()> {
        let clock = Clock::get()?;
        let cfg = &ctx.accounts.config;
        require_keys_eq!(ctx.accounts.treasury.key(), cfg.treasury, FairError::WrongTreasury);
        let l = &mut ctx.accounts.launch;
        require!(!l.settled, FairError::AlreadySettled);
        require!(clock.slot >= l.auction_end_slot, FairError::AuctionRunning);

        let total = l.total_committed;
        let mut fee = 0u64;
        l.settled = true;
        l.trading_start_slot = clock.slot;
        l.vest_start = clock.unix_timestamp;

        if total < l.min_raise {
            // Below the guest-list minimum: nobody trades, everyone gets their SOL back.
            l.cancelled = true;
        } else if total > 0 {
            let gross_fee = math::fee_amount(total, cfg.base_fee_bps).fx()?;
            let mut q = math::quote_buy(l.virtual_sol, l.virtual_token, l.real_token, total - gross_fee).fx()?;
            // A line big enough to fill the whole curve stops just short of it while the cover
            // charge is on; the unused SOL is refunded to the line like any overfill.
            if q.completes_curve && in_cover_window(cfg, l, clock.slot) && l.real_token > SETTLE_SLIVER_TOKENS {
                let tokens_out = l.real_token - SETTLE_SLIVER_TOKENS;
                let sol = math::sol_for_exact_tokens(l.virtual_sol as u128, l.virtual_token as u128, tokens_out as u128).fx()?;
                q = math::BuyQuote { tokens_out, sol_used: u64::try_from(sol).map_err(|_| error!(FairError::Overflow))?.min(total - gross_fee), completes_curve: false };
                fee = math::fee_amount(q.sol_used, cfg.base_fee_bps).fx()?;
            } else {
                fee = if q.completes_curve { math::fee_amount(q.sol_used, cfg.base_fee_bps).fx()? } else { gross_fee };
            }
            // Big lines unlock gradually so a full line can't dump the moment the doors open.
            let ext = &ctx.accounts.config_ext.params;
            if ext.big_line_vest_secs > 0 && q.sol_used > ext.big_line_sol {
                l.line_unlock_bps = ext.big_line_unlock_bps;
                l.line_vest_secs = ext.big_line_vest_secs;
            }
            let refund = total - q.sol_used - fee;
            l.virtual_sol += q.sol_used;
            l.virtual_token -= q.tokens_out;
            l.real_sol += q.sol_used;
            l.real_token -= q.tokens_out;
            l.batch_tokens = q.tokens_out;
            l.unclaimed_batch = q.tokens_out;
            l.batch_refund = refund;
            l.escrow_sol = refund;
            if q.completes_curve {
                l.complete = true;
            }
            route_fee(cfg, l, fee, cfg.base_fee_bps, None, &ctx.accounts.treasury.to_account_info())?;
        }
        emit!(AuctionSettled {
            launch: l.key(),
            total_committed: total,
            batch_tokens: l.batch_tokens,
            batch_refund: l.batch_refund,
            fee,
            trading_start_slot: clock.slot,
            cancelled: l.cancelled,
        });
        Ok(())
    }

    /// Claim your share of the line's tokens (plus overfill refund), or your full
    /// refund if the launch was cancelled. Closes the commitment.
    pub fn claim(ctx: Context<Claim>) -> Result<()> {
        let l = &mut ctx.accounts.launch;
        require!(l.settled, FairError::NotSettled);
        let info = ctx.accounts.commitment.to_account_info();
        let mut c = read_commitment(&info)?;
        require!(c.owner == ctx.accounts.user.key() && c.launch == l.key(), FairError::BadCommitment);
        let amt = c.amount;
        let (share, refund_total) = if l.cancelled {
            (0, amt)
        } else {
            (
                math::pro_rata(l.batch_tokens, amt, l.total_committed).fx()?,
                math::pro_rata(l.batch_refund, amt, l.total_committed).fx()?,
            )
        };
        let now = Clock::get()?.unix_timestamp;
        let unlocked = math::line_unlocked(share, l.line_unlock_bps, l.line_vest_secs, now.saturating_sub(l.vest_start)).fx()?;
        let tokens = unlocked.saturating_sub(c.claimed);
        let refund = if c.refunded { 0 } else { refund_total };
        require!(tokens > 0 || refund > 0 || share == 0, FairError::NothingToClaim);
        if tokens > 0 {
            launch_signer!(l, signer);
            token::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer { from: ctx.accounts.vault.to_account_info(), to: ctx.accounts.user_ata.to_account_info(), authority: l.to_account_info() },
                    signer,
                ),
                tokens,
            )?;
            l.unclaimed_batch = l.unclaimed_batch.saturating_sub(tokens);
        }
        if refund > 0 {
            l.escrow_sol = l.escrow_sol.checked_sub(refund).ok_or(FairError::Overflow)?;
            pay_from_pda(&l.to_account_info(), &ctx.accounts.user.to_account_info(), refund)?;
        }
        // Velvet Pass: a line counts once, on its first claim, if it went ahead and the wallet put
        // in enough.
        let first = c.claimed == 0 && !c.refunded;
        touch_pass(&mut ctx.accounts.pass, ctx.accounts.user.key(), ctx.bumps.pass);
        if first && !l.cancelled && share > 0 && amt.saturating_sub(refund_total) >= PASS_MIN_LINE_SOL {
            let p = &mut ctx.accounts.pass;
            p.lines = p.lines.saturating_add(1);
            p.sol_in_lines = p.sol_in_lines.saturating_add(amt.saturating_sub(refund_total));
            if p.flip_pending {
                p.clean_since_flip = p.clean_since_flip.saturating_add(1);
                if p.clean_since_flip >= PASS_HEAL_LINES { p.flip_pending = false; }
            }
            emit!(PassUpdated { owner: p.owner, lines: p.lines, sol_in_lines: p.sol_in_lines, flips: p.flips, tier: p.tier() });
        }
        c.claimed = c.claimed.checked_add(tokens).ok_or(FairError::Overflow)?;
        c.refunded = true;
        if c.claimed >= share {
            close_account(&info, &ctx.accounts.user.to_account_info())?;
        } else {
            write_commitment(&info, &c, &ctx.accounts.user.to_account_info(), &ctx.accounts.system_program.to_account_info())?;
        }
        emit!(Claimed { launch: l.key(), user: ctx.accounts.user.key(), tokens, refund });
        Ok(())
    }

    /// Creator takes whatever part of their opening buy has vested.
    pub fn claim_vested(ctx: Context<CreatorEscrow>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let l = &mut ctx.accounts.launch;
        require!(l.settled, FairError::NotSettled);
        require!(!l.cancelled, FairError::LaunchCancelled);
        let unlocked = math::vested(l.creator_tokens, l.vest_start, l.vest_secs, now);
        let amount = unlocked.saturating_sub(l.creator_claimed);
        require!(amount > 0, FairError::NothingToClaim);
        launch_signer!(l, signer);
        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer { from: ctx.accounts.escrow.to_account_info(), to: ctx.accounts.creator_ata.to_account_info(), authority: l.to_account_info() },
                signer,
            ),
            amount,
        )?;
        l.creator_claimed += amount;
        emit!(CreatorVested { launch: l.key(), amount, total_claimed: l.creator_claimed });
        Ok(())
    }

    /// Cancelled launch: the creator's opening buy is undone (minus its fee).
    pub fn refund_creator(ctx: Context<CreatorEscrow>) -> Result<()> {
        let l = &mut ctx.accounts.launch;
        require!(l.cancelled, FairError::NotCancelled);
        let tokens = l.creator_tokens;
        let sol = l.creator_buy_sol;
        require!(tokens > 0 || sol > 0, FairError::NothingToClaim);
        launch_signer!(l, signer);
        if tokens > 0 {
            token::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer { from: ctx.accounts.escrow.to_account_info(), to: ctx.accounts.vault.to_account_info(), authority: l.to_account_info() },
                    signer,
                ),
                tokens,
            )?;
        }
        l.real_sol = l.real_sol.checked_sub(sol).ok_or(FairError::Overflow)?;
        pay_from_pda(&l.to_account_info(), &ctx.accounts.creator.to_account_info(), sol)?;
        l.creator_tokens = 0;
        l.creator_buy_sol = 0;
        emit!(CreatorRefunded { launch: l.key(), sol, tokens_returned: tokens });
        Ok(())
    }

    pub fn buy(ctx: Context<TradeCtx>, lamports_in: u64, min_tokens_out: u64) -> Result<()> {
        require!(lamports_in > 0, FairError::ZeroAmount);
        let slot = Clock::get()?.slot;
        let cfg = &ctx.accounts.config;
        require_keys_eq!(ctx.accounts.treasury.key(), cfg.treasury, FairError::WrongTreasury);
        let l = &mut ctx.accounts.launch;
        require!(l.settled, FairError::NotSettled);
        require!(!l.cancelled, FairError::LaunchCancelled);
        require!(!l.complete && !l.graduated, FairError::CurveComplete);

        touch_pass(&mut ctx.accounts.pass, ctx.accounts.user.key(), ctx.bumps.pass);
        let fee_bps = fee_for(cfg, l, slot, &ctx.accounts.pass);
        let gross_fee = math::fee_amount(lamports_in, fee_bps).fx()?;
        let q = math::quote_buy(l.virtual_sol, l.virtual_token, l.real_token, lamports_in - gross_fee).fx()?;
        let fee = if q.completes_curve { math::fee_amount(q.sol_used, fee_bps).fx()? } else { gross_fee };
        require!(!(q.completes_curve && in_cover_window(cfg, l, slot)), FairError::CurveFillTooEarly);
        require!(q.tokens_out >= min_tokens_out, FairError::Slippage);
        // Anti-whale while bonding: no curve buy may take a wallet above 3% of supply. Tokens the
        // wallet is still owed count too: an unclaimed line share and, for the creator, the bag
        // still in escrow.
        let pending_line = {
            let info = ctx.accounts.commitment.to_account_info();
            if info.owner == &crate::ID && !info.data_is_empty() && !l.cancelled && l.total_committed > 0 {
                let c = read_commitment(&info)?;
                math::pro_rata(l.batch_tokens, c.amount, l.total_committed).fx()?.saturating_sub(c.claimed)
            } else {
                0
            }
        };
        let in_escrow = if ctx.accounts.user.key() == l.creator { l.creator_tokens.saturating_sub(l.creator_claimed) } else { 0 };
        let held_after = ctx.accounts.user_ata.amount
            .checked_add(q.tokens_out).ok_or(FairError::Overflow)?
            .checked_add(pending_line).ok_or(FairError::Overflow)?
            .checked_add(in_escrow).ok_or(FairError::Overflow)?;
        require!(held_after <= MAX_WALLET_TOKENS_BONDING, FairError::WalletCapExceeded);

        let system = ctx.accounts.system_program.to_account_info();
        let user = ctx.accounts.user.to_account_info();
        pay_from_user(&system, &user, &l.to_account_info(), q.sol_used)?;
        let to_holders = route_fee(cfg, l, fee, fee_bps, Some((&system, &user)), &ctx.accounts.treasury.to_account_info())?;
        launch_signer!(l, signer);
        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer { from: ctx.accounts.vault.to_account_info(), to: ctx.accounts.user_ata.to_account_info(), authority: l.to_account_info() },
                signer,
            ),
            q.tokens_out,
        )?;
        l.virtual_sol += q.sol_used;
        l.virtual_token -= q.tokens_out;
        l.real_sol += q.sol_used;
        l.real_token -= q.tokens_out;
        if q.completes_curve {
            l.complete = true;
        }
        emit!(state::Trade {
            launch: l.key(), user: user.key(), is_buy: true, sol_amount: q.sol_used, token_amount: q.tokens_out,
            fee, fee_bps, to_holders, virtual_sol: l.virtual_sol, virtual_token: l.virtual_token,
        });
        Ok(())
    }

    pub fn sell(ctx: Context<TradeCtx>, tokens_in: u64, min_lamports_out: u64) -> Result<()> {
        let slot = Clock::get()?.slot;
        let cfg = &ctx.accounts.config;
        require_keys_eq!(ctx.accounts.treasury.key(), cfg.treasury, FairError::WrongTreasury);
        let l = &mut ctx.accounts.launch;
        require!(l.settled, FairError::NotSettled);
        require!(!l.cancelled, FairError::LaunchCancelled);
        require!(!l.complete && !l.graduated, FairError::CurveComplete);

        let gross = math::quote_sell(l.virtual_sol, l.virtual_token, tokens_in).fx()?.min(l.real_sol);
        touch_pass(&mut ctx.accounts.pass, ctx.accounts.user.key(), ctx.bumps.pass);
        let fee_bps = fee_for(cfg, l, slot, &ctx.accounts.pass);
        // Selling while the cover charge is on is a flip: it costs a tier until it heals.
        if fee_now(cfg, l, slot) > cfg.base_fee_bps {
            let p = &mut ctx.accounts.pass;
            p.flips = p.flips.saturating_add(1);
            p.flip_pending = true;
            p.clean_since_flip = 0;
            emit!(PassUpdated { owner: p.owner, lines: p.lines, sol_in_lines: p.sol_in_lines, flips: p.flips, tier: p.tier() });
        }
        let fee = math::fee_amount(gross, fee_bps).fx()?;
        let out = gross.checked_sub(fee).ok_or(FairError::Overflow)?;
        require!(out >= min_lamports_out, FairError::Slippage);

        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer { from: ctx.accounts.user_ata.to_account_info(), to: ctx.accounts.vault.to_account_info(), authority: ctx.accounts.user.to_account_info() },
            ),
            tokens_in,
        )?;
        l.virtual_sol -= gross;
        l.virtual_token += tokens_in;
        l.real_sol -= gross;
        l.real_token += tokens_in;
        // The fee stays in the launch account first, then is routed.
        pay_from_pda(&l.to_account_info(), &ctx.accounts.user.to_account_info(), out)?;
        let to_holders = route_fee(cfg, l, fee, fee_bps, None, &ctx.accounts.treasury.to_account_info())?;
        emit!(state::Trade {
            launch: l.key(), user: ctx.accounts.user.key(), is_buy: false, sol_amount: out, token_amount: tokens_in,
            fee, fee_bps, to_holders, virtual_sol: l.virtual_sol, virtual_token: l.virtual_token,
        });
        Ok(())
    }

    /// Hold-streak payout. The distributor computes each holder's share off-chain
    /// and pushes SOL from this launch's reward pool. Every recipient must be passed
    /// as (token account, wallet) and must currently hold the coin.
    ///
    /// The program limits what the distributor key can do even if it is stolen:
    /// epochs are at least `min_reward_epoch_secs` apart, an epoch can pay out at most
    /// the pool as it stood when the epoch started, and nobody can receive more than
    /// MAX_STREAK_MULT times their share of circulating supply.
    pub fn distribute_rewards<'info>(ctx: Context<'_, '_, 'info, 'info, Distribute<'info>>, epoch: u32, amounts: Vec<u64>) -> Result<()> {
        let cfg = &ctx.accounts.config;
        require!(cfg.distributor != Pubkey::default(), FairError::NotDistributor);
        require_keys_eq!(ctx.accounts.distributor.key(), cfg.distributor, FairError::NotDistributor);
        let now = Clock::get()?.unix_timestamp;
        let l = &mut ctx.accounts.launch;
        require!(l.settled && !l.cancelled, FairError::NotSettled);
        let rem = ctx.remaining_accounts;
        require!(!amounts.is_empty() && rem.len() == amounts.len() * 2, FairError::BadRewardAccount);
        let total: u64 = amounts.iter().try_fold(0u64, |a, x| a.checked_add(*x)).ok_or(FairError::Overflow)?;
        require!(total <= l.reward_sol, FairError::RewardOverdraw);

        // Epoch guard.
        if epoch > l.reward_epochs || l.epoch_started_at == 0 {
            require!(epoch > l.reward_epochs || (l.reward_epochs == 0 && epoch == 0), FairError::BadEpoch);
            if l.epoch_started_at != 0 {
                require!(now >= l.epoch_started_at.checked_add(cfg.min_reward_epoch_secs).ok_or(FairError::Overflow)?, FairError::EpochTooSoon);
            }
            l.reward_epochs = epoch;
            l.epoch_started_at = now;
            l.epoch_budget = l.reward_sol;
            l.epoch_paid = 0;
        } else {
            require!(epoch == l.reward_epochs, FairError::BadEpoch);
            require!(now <= l.epoch_started_at.checked_add(EPOCH_BATCH_WINDOW_SECS).ok_or(FairError::Overflow)?, FairError::EpochWindowClosed);
        }
        let paid = l.epoch_paid.checked_add(total).ok_or(FairError::Overflow)?;
        require!(paid <= l.epoch_budget, FairError::EpochBudgetExceeded);

        // Circulating supply: everything not held by the curve vault, the creator
        // escrow or the Raydium pool.
        let mut locked = (ctx.accounts.vault.amount as u128) + (ctx.accounts.escrow.amount as u128);
        if l.graduated {
            let pv = ctx.accounts.pool_token_vault.as_ref().ok_or(FairError::WrongPoolAccount)?;
            require_keys_eq!(pv.key(), raydium::vault(&cfg.cpmm_program, &l.pool, &l.mint), FairError::WrongPoolAccount);
            locked += token_amount(&pv.to_account_info())? as u128;
        }
        let circulating = (ctx.accounts.mint.supply as u128).saturating_sub(locked);
        require!(circulating > 0, FairError::BadRewardAccount);
        let budget = l.epoch_budget as u128;

        let launch_info = l.to_account_info();
        for (i, amount) in amounts.iter().enumerate() {
            let token_acc = &rem[i * 2];
            let wallet = &rem[i * 2 + 1];
            require_keys_eq!(*token_acc.owner, token::ID, FairError::BadRewardAccount);
            require!(wallet.key() != launch_info.key(), FairError::BadRewardAccount);
            let data = token_acc.try_borrow_data()?;
            let ta = spl_token::state::Account::unpack_from_slice(&data).map_err(|_| error!(FairError::BadRewardAccount))?;
            drop(data);
            require!(ta.mint == l.mint && ta.owner == wallet.key() && ta.amount > 0, FairError::BadRewardAccount);
            require!(wallet.is_writable, FairError::BadRewardAccount);
            // amount / budget <= MAX_STREAK_MULT * balance / circulating
            require!(
                (*amount as u128) * circulating <= MAX_STREAK_MULT * (ta.amount as u128) * budget,
                FairError::RewardAboveShare
            );
            pay_from_pda(&launch_info, wallet, *amount)?;
        }
        l.epoch_paid = paid;
        l.reward_sol -= total;
        l.reward_paid = l.reward_paid.checked_add(total).ok_or(FairError::Overflow)?;
        emit!(RewardsPaid { launch: l.key(), epoch, recipients: amounts.len() as u32, total, remaining: l.reward_sol });
        Ok(())
    }

    /// Permissionless, step 1 of graduation (same transaction as `graduate`):
    /// pay the graduation fee and move the curve's SOL to the graduation authority.
    /// Kept separate because lamports moved by hand can't be followed by a CPI in
    /// the same instruction.
    pub fn prepare_graduation(ctx: Context<PrepareGraduation>) -> Result<()> {
        let cfg = &ctx.accounts.config;
        require_keys_eq!(ctx.accounts.treasury.key(), cfg.treasury, FairError::WrongTreasury);
        let l = &mut ctx.accounts.launch;
        require!(l.complete, FairError::NotComplete);
        require!(!l.graduated && !l.grad_prepared, FairError::AlreadyGraduated);
        let grad_fee = cfg.graduation_fee.min(l.real_sol);
        let available = l.real_sol - grad_fee;
        require!(available > GRAD_OVERHEAD + MIN_POOL_SOL, FairError::GraduationUnderfunded);
        pay_from_pda(&l.to_account_info(), &ctx.accounts.treasury.to_account_info(), grad_fee)?;
        pay_from_pda(&l.to_account_info(), &ctx.accounts.grad_auth.to_account_info(), available)?;
        l.real_sol = 0;
        l.grad_pool_sol = available - GRAD_OVERHEAD;
        l.grad_fee_paid = grad_fee;
        l.grad_prepared = true;
        Ok(())
    }

    /// Permissionless, step 2: create the Raydium CPMM pool.
    pub fn graduate(ctx: Context<Graduate>) -> Result<()> {
        let cfg = &ctx.accounts.config;
        let a = &ctx.accounts;
        require_keys_eq!(a.treasury.key(), cfg.treasury, FairError::WrongTreasury);
        {
            let l = &a.launch;
            require!(l.grad_prepared, FairError::NotComplete);
            require!(!l.graduated, FairError::AlreadyGraduated);
        }
        let launch_key = a.launch.key();
        let mint_key = a.mint.key();
        let cpmm = cfg.cpmm_program;
        let wsol = spl_token::native_mint::ID;
        let (mint0, mint1, token_is_1) = raydium::sorted(mint_key, wsol);
        // The pool lives at a fresh keypair address (it signs this transaction), never
        // at Raydium's predictable PDA, so nobody can create the pool first and block
        // graduation.
        let pool = a.pool_state.key();
        require!(pool != raydium::pool_state(&cpmm, &cfg.amm_config, &mint0, &mint1), FairError::WrongPoolAccount);
        require_keys_eq!(a.ray_authority.key(), raydium::authority(&cpmm), FairError::WrongPoolAccount);
        require_keys_eq!(a.lp_mint.key(), raydium::lp_mint(&cpmm, &pool), FairError::WrongPoolAccount);
        require_keys_eq!(a.token_0_vault.key(), raydium::vault(&cpmm, &pool, &mint0), FairError::WrongPoolAccount);
        require_keys_eq!(a.token_1_vault.key(), raydium::vault(&cpmm, &pool, &mint1), FairError::WrongPoolAccount);
        require_keys_eq!(a.observation.key(), raydium::observation(&cpmm, &pool), FairError::WrongPoolAccount);
        let grad = a.grad_auth.key();
        require_keys_eq!(a.grad_wsol.key(), associated_token::get_associated_token_address(&grad, &wsol), FairError::WrongPoolAccount);
        require_keys_eq!(a.grad_token.key(), associated_token::get_associated_token_address(&grad, &mint_key), FairError::WrongPoolAccount);
        require_keys_eq!(a.grad_lp.key(), associated_token::get_associated_token_address(&grad, &a.lp_mint.key()), FairError::WrongPoolAccount);

        let grad_bump = a.launch.grad_bump;
        grad_signer!(launch_key, grad_bump, gsig);
        let system = a.system_program.to_account_info();
        let token_prog = a.token_program.to_account_info();

        // 1. SOL is already with the graduation authority (prepare_graduation).
        let lp_reserve = a.launch.lp_reserve;
        let pool_sol = a.launch.grad_pool_sol;
        let grad_fee = a.launch.grad_fee_paid;

        // 2. Token accounts for the graduation authority.
        for (ata, m) in [(a.grad_wsol.to_account_info(), a.wsol_mint.to_account_info()), (a.grad_token.to_account_info(), a.mint.to_account_info())] {
            associated_token::create_idempotent(CpiContext::new_with_signer(
                a.associated_token_program.to_account_info(),
                associated_token::Create {
                    payer: a.grad_auth.to_account_info(),
                    associated_token: ata,
                    authority: a.grad_auth.to_account_info(),
                    mint: m,
                    system_program: system.clone(),
                    token_program: token_prog.clone(),
                },
                gsig,
            ))?;
        }
        // 3. Wrap the pool's SOL.
        pay_from_system_pda(&system, &a.grad_auth.to_account_info(), &a.grad_wsol.to_account_info(), pool_sol, gsig)?;
        token::sync_native(CpiContext::new(token_prog.clone(), token::SyncNative { account: a.grad_wsol.to_account_info() }))?;
        // 4. Move the LP reserve out of the vault.
        {
            let l = &a.launch;
            launch_signer!(l, lsig);
            token::transfer(
                CpiContext::new_with_signer(
                    token_prog.clone(),
                    Transfer { from: a.vault.to_account_info(), to: a.grad_token.to_account_info(), authority: l.to_account_info() },
                    lsig,
                ),
                lp_reserve,
            )?;
        }
        // 5. Create the Raydium pool.
        let (amt0, amt1, ct0, ct1) = if token_is_1 {
            (pool_sol, lp_reserve, a.grad_wsol.key(), a.grad_token.key())
        } else {
            (lp_reserve, pool_sol, a.grad_token.key(), a.grad_wsol.key())
        };
        let keys = raydium::InitializeKeys {
            program: cpmm,
            creator: grad,
            amm_config: cfg.amm_config,
            authority: a.ray_authority.key(),
            pool_state: pool,
            token_0_mint: mint0,
            token_1_mint: mint1,
            lp_mint: a.lp_mint.key(),
            creator_token_0: ct0,
            creator_token_1: ct1,
            creator_lp_token: a.grad_lp.key(),
            token_0_vault: a.token_0_vault.key(),
            token_1_vault: a.token_1_vault.key(),
            create_pool_fee: a.create_pool_fee.key(),
            observation_state: a.observation.key(),
            token_program: token::ID,
            associated_token_program: AssociatedToken::id(),
            system_program: system_program::ID,
            rent: anchor_lang::solana_program::sysvar::rent::ID,
        };
        let (m0_info, m1_info) = if token_is_1 { (a.wsol_mint.to_account_info(), a.mint.to_account_info()) } else { (a.mint.to_account_info(), a.wsol_mint.to_account_info()) };
        let (c0_info, c1_info) = if token_is_1 { (a.grad_wsol.to_account_info(), a.grad_token.to_account_info()) } else { (a.grad_token.to_account_info(), a.grad_wsol.to_account_info()) };
        invoke_signed(
            &raydium::initialize_ix(&keys, amt0, amt1, 0),
            &[
                a.grad_auth.to_account_info(), a.amm_config.to_account_info(), a.ray_authority.to_account_info(),
                a.pool_state.to_account_info(), m0_info, m1_info, a.lp_mint.to_account_info(), c0_info, c1_info,
                a.grad_lp.to_account_info(), a.token_0_vault.to_account_info(), a.token_1_vault.to_account_info(),
                a.create_pool_fee.to_account_info(), a.observation.to_account_info(), token_prog.clone(),
                a.associated_token_program.to_account_info(), system.clone(), a.rent.to_account_info(), a.cpmm_program.to_account_info(),
            ],
            gsig,
        )?;
        // 6. The LP tokens stay with the graduation authority until `lock_liquidity`
        //    hands them to Raydium's locker (no instruction here can move them anywhere else).
        // 7. Leftover overhead (unused rent budget) to treasury; keep a float for fee collection.
        let left = a.grad_auth.lamports().saturating_sub(GRAD_AUTH_FLOAT);
        pay_from_system_pda(&system, &a.grad_auth.to_account_info(), &a.treasury.to_account_info(), left, gsig)?;

        let l = &mut ctx.accounts.launch;
        l.graduated = true;
        l.pool = pool;
        l.graduated_slot = Clock::get()?.slot;
        emit!(Graduated { launch: launch_key, pool, sol_to_pool: pool_sol, tokens_to_pool: lp_reserve, graduation_fee: grad_fee });
        Ok(())
    }

    /// Permissionless, step 3 (second transaction): lock all LP with Raydium's
    /// locker. The liquidity can never be withdrawn; the fee-key NFT goes to the
    /// graduation authority so only this program can collect the pool fees.
    pub fn lock_liquidity(ctx: Context<LockLiquidity>) -> Result<()> {
        let cfg = &ctx.accounts.config;
        let a = &ctx.accounts;
        require!(a.launch.graduated, FairError::NotGraduated);
        require!(!a.launch.lp_locked, FairError::AlreadyGraduated);
        require_keys_eq!(a.pool_state.key(), a.launch.pool, FairError::WrongPoolAccount);
        let lockp = cfg.lock_program;
        let cpmm = cfg.cpmm_program;
        let pool = a.launch.pool;
        let grad = a.grad_auth.key();
        let lp_mint = raydium::lp_mint(&cpmm, &pool);
        let (mint0, mint1, _) = raydium::sorted(a.launch.mint, spl_token::native_mint::ID);
        require_keys_eq!(a.lp_mint.key(), lp_mint, FairError::WrongPoolAccount);
        require_keys_eq!(a.grad_lp.key(), associated_token::get_associated_token_address(&grad, &lp_mint), FairError::WrongPoolAccount);
        require_keys_eq!(a.token_0_vault.key(), raydium::vault(&cpmm, &pool, &mint0), FairError::WrongPoolAccount);
        require_keys_eq!(a.token_1_vault.key(), raydium::vault(&cpmm, &pool, &mint1), FairError::WrongPoolAccount);
        require_keys_eq!(a.lock_authority.key(), raydium::lock::authority(&lockp), FairError::WrongPoolAccount);
        require_keys_eq!(a.locked_liquidity.key(), raydium::lock::locked_liquidity(&lockp, &a.fee_nft_mint.key()), FairError::WrongPoolAccount);
        require_keys_eq!(a.locked_lp_vault.key(), associated_token::get_associated_token_address(&a.lock_authority.key(), &lp_mint), FairError::WrongPoolAccount);
        require_keys_eq!(a.fee_nft_account.key(), associated_token::get_associated_token_address(&grad, &a.fee_nft_mint.key()), FairError::WrongPoolAccount);

        let lp_amount = token_amount(&a.grad_lp.to_account_info())?;
        let launch_key = a.launch.key();
        let grad_bump = a.launch.grad_bump;
        grad_signer!(launch_key, grad_bump, gsig);
        let keys = raydium::lock::LockKeys {
            program: lockp,
            authority: a.lock_authority.key(),
            payer: a.payer.key(),
            liquidity_owner: grad,
            fee_nft_owner: grad,
            fee_nft_mint: a.fee_nft_mint.key(),
            fee_nft_account: a.fee_nft_account.key(),
            pool_state: pool,
            locked_liquidity: a.locked_liquidity.key(),
            lp_mint,
            liquidity_owner_lp: a.grad_lp.key(),
            locked_lp_vault: a.locked_lp_vault.key(),
            token_0_vault: a.token_0_vault.key(),
            token_1_vault: a.token_1_vault.key(),
            metadata_account: a.nft_metadata.key(),
            metadata_program: a.metadata_program.key(),
        };
        invoke_signed(
            &raydium::lock::lock_cp_liquidity_ix(&keys, lp_amount),
            &[
                a.lock_authority.to_account_info(), a.payer.to_account_info(), a.grad_auth.to_account_info(),
                a.fee_nft_mint.to_account_info(), a.fee_nft_account.to_account_info(), a.pool_state.to_account_info(),
                a.locked_liquidity.to_account_info(), a.lp_mint.to_account_info(), a.grad_lp.to_account_info(),
                a.locked_lp_vault.to_account_info(), a.token_0_vault.to_account_info(), a.token_1_vault.to_account_info(),
                a.nft_metadata.to_account_info(), a.rent.to_account_info(), a.system_program.to_account_info(),
                a.token_program.to_account_info(), a.associated_token_program.to_account_info(),
                a.metadata_program.to_account_info(), a.lock_program.to_account_info(),
            ],
            gsig,
        )?;
        let fee_nft_mint = a.fee_nft_mint.key();
        let l = &mut ctx.accounts.launch;
        l.lp_locked = true;
        l.fee_nft_mint = fee_nft_mint;
        emit!(LiquidityLocked { launch: launch_key, pool, lp_amount, fee_nft_mint });
        Ok(())
    }

    /// Permissionless. Collect the locked LP's trading fees through the fee key:
    /// SOL is split between treasury, creator and the holder pool. A quarter of the
    /// treasury share, paired with the coin side, goes back into the pool as permanent
    /// liquidity; any coin left over is burned.
    pub fn collect_pool_fees(ctx: Context<CollectPoolFees>) -> Result<()> {
        let cfg = &ctx.accounts.config;
        let a = &ctx.accounts;
        require_keys_eq!(a.treasury.key(), cfg.treasury, FairError::WrongTreasury);
        require!(a.launch.lp_locked, FairError::NotGraduated);
        require_keys_eq!(a.pool_state.key(), a.launch.pool, FairError::WrongPoolAccount);
        require_keys_eq!(a.creator.key(), a.launch.creator, FairError::NotCreator);
        let cpmm = cfg.cpmm_program;
        let lockp = cfg.lock_program;
        let wsol = spl_token::native_mint::ID;
        let mint_key = a.mint.key();
        let (mint0, mint1, token_is_1) = raydium::sorted(mint_key, wsol);
        let pool = a.launch.pool;
        let grad = a.grad_auth.key();
        let lp_mint = raydium::lp_mint(&cpmm, &pool);
        let nft = a.launch.fee_nft_mint;
        require_keys_eq!(a.lp_mint.key(), lp_mint, FairError::WrongPoolAccount);
        require_keys_eq!(a.token_0_vault.key(), raydium::vault(&cpmm, &pool, &mint0), FairError::WrongPoolAccount);
        require_keys_eq!(a.token_1_vault.key(), raydium::vault(&cpmm, &pool, &mint1), FairError::WrongPoolAccount);
        require_keys_eq!(a.grad_wsol.key(), associated_token::get_associated_token_address(&grad, &wsol), FairError::WrongPoolAccount);
        require_keys_eq!(a.grad_token.key(), associated_token::get_associated_token_address(&grad, &mint_key), FairError::WrongPoolAccount);
        require_keys_eq!(a.lock_authority.key(), raydium::lock::authority(&lockp), FairError::WrongPoolAccount);
        require_keys_eq!(a.locked_liquidity.key(), raydium::lock::locked_liquidity(&lockp, &nft), FairError::WrongPoolAccount);
        require_keys_eq!(a.fee_nft_account.key(), associated_token::get_associated_token_address(&grad, &nft), FairError::WrongPoolAccount);
        require_keys_eq!(a.locked_lp_vault.key(), associated_token::get_associated_token_address(&a.lock_authority.key(), &lp_mint), FairError::WrongPoolAccount);

        let launch_key = a.launch.key();
        let grad_bump = a.launch.grad_bump;
        grad_signer!(launch_key, grad_bump, gsig);
        let system = a.system_program.to_account_info();
        let token_prog = a.token_program.to_account_info();

        // Recipient accounts (the WSOL one is closed after every collection).
        for (ata, m) in [(a.grad_wsol.to_account_info(), a.wsol_mint.to_account_info()), (a.grad_token.to_account_info(), a.mint.to_account_info())] {
            associated_token::create_idempotent(CpiContext::new(
                a.associated_token_program.to_account_info(),
                associated_token::Create {
                    payer: a.payer.to_account_info(),
                    associated_token: ata,
                    authority: a.grad_auth.to_account_info(),
                    mint: m,
                    system_program: system.clone(),
                    token_program: token_prog.clone(),
                },
            ))?;
        }
        let (r0_key, r1_key) = if token_is_1 { (a.grad_wsol.key(), a.grad_token.key()) } else { (a.grad_token.key(), a.grad_wsol.key()) };
        let keys = raydium::lock::CollectKeys {
            program: lockp,
            authority: a.lock_authority.key(),
            fee_nft_owner: grad,
            fee_nft_account: a.fee_nft_account.key(),
            locked_liquidity: a.locked_liquidity.key(),
            cpmm_program: cpmm,
            cp_authority: raydium::authority(&cpmm),
            pool_state: pool,
            lp_mint,
            recipient_token_0: r0_key,
            recipient_token_1: r1_key,
            token_0_vault: a.token_0_vault.key(),
            token_1_vault: a.token_1_vault.key(),
            vault_0_mint: mint0,
            vault_1_mint: mint1,
            locked_lp_vault: a.locked_lp_vault.key(),
        };
        let (m0_info, m1_info) = if token_is_1 { (a.wsol_mint.to_account_info(), a.mint.to_account_info()) } else { (a.mint.to_account_info(), a.wsol_mint.to_account_info()) };
        let (r0_info, r1_info) = if token_is_1 { (a.grad_wsol.to_account_info(), a.grad_token.to_account_info()) } else { (a.grad_token.to_account_info(), a.grad_wsol.to_account_info()) };
        invoke_signed(
            &raydium::lock::collect_cp_fees_ix(&keys),
            &[
                a.lock_authority.to_account_info(), a.grad_auth.to_account_info(), a.fee_nft_account.to_account_info(),
                a.locked_liquidity.to_account_info(), a.cpmm_program.to_account_info(), a.ray_authority.to_account_info(),
                a.pool_state.to_account_info(), a.lp_mint.to_account_info(), r0_info, r1_info,
                a.token_0_vault.to_account_info(), a.token_1_vault.to_account_info(), m0_info, m1_info,
                a.locked_lp_vault.to_account_info(), token_prog.clone(), a.token_2022_program.to_account_info(),
                a.memo_program.to_account_info(), a.lock_program.to_account_info(),
            ],
            gsig,
        )?;

        let sol = token_amount(&a.grad_wsol.to_account_info())?;
        let tokens = token_amount(&a.grad_token.to_account_info())?;
        let (to_creator, to_holders, treasury_share) = math::split3(sol, cfg.pool_fee_creator_bps, cfg.pool_fee_holders_bps);

        // Recycle: a quarter of Velvet's share, paired with the collected coin side, goes
        // back into the pool as liquidity. The LP tokens are burned, so it stays there forever.
        let budget = (treasury_share as u128 * RECYCLE_TREASURY_BPS as u128 / 10_000) as u64;
        let (lp, sol_used, _) = recycle_liquidity(a, budget, tokens, token_is_1, (mint0, mint1), (r0_key, r1_key), gsig)?;
        let to_treasury = treasury_share - if lp > 0 { sol_used } else { 0 };

        // SOL side: unwrap what's left by closing the WSOL account into the graduation authority.
        token::close_account(CpiContext::new_with_signer(
            token_prog.clone(),
            token::CloseAccount { account: a.grad_wsol.to_account_info(), destination: a.grad_auth.to_account_info(), authority: a.grad_auth.to_account_info() },
            gsig,
        ))?;
        pay_from_system_pda(&system, &a.grad_auth.to_account_info(), &a.creator.to_account_info(), to_creator, gsig)?;
        pay_from_system_pda(&system, &a.grad_auth.to_account_info(), &a.launch.to_account_info(), to_holders, gsig)?;
        pay_from_system_pda(&system, &a.grad_auth.to_account_info(), &a.treasury.to_account_info(), to_treasury, gsig)?;

        // Coin side not used for liquidity: burn it.
        let burned = token_amount(&a.grad_token.to_account_info())?;
        if burned > 0 {
            token::burn(
                CpiContext::new_with_signer(
                    token_prog,
                    token::Burn { mint: a.mint.to_account_info(), from: a.grad_token.to_account_info(), authority: a.grad_auth.to_account_info() },
                    gsig,
                ),
                burned,
            )?;
        }
        let l = &mut ctx.accounts.launch;
        l.reward_sol = l.reward_sol.checked_add(to_holders).ok_or(FairError::Overflow)?;
        l.pool_fees_collected = l.pool_fees_collected.checked_add(sol).ok_or(FairError::Overflow)?;
        emit!(PoolFeesCollected { launch: launch_key, sol, to_treasury, to_creator, to_holders, tokens_burned: burned });
        Ok(())
    }
}


/// Pair `budget` lamports of wrapped SOL with up to `tokens` of the coin (both already
/// in the graduation authority's accounts) and deposit them into the coin's Raydium
/// pool, then burn the LP tokens so the liquidity can never leave. Returns
/// (lp, sol_used, tokens_used); all zero if the amounts are too small to deposit.
/// Kept out of `collect_pool_fees` so each function stays within the stack limit.
#[inline(never)]
fn recycle_liquidity<'info>(
    a: &CollectPoolFees<'info>,
    budget: u64,
    tokens: u64,
    token_is_1: bool,
    (mint0, mint1): (Pubkey, Pubkey),
    (r0_key, r1_key): (Pubkey, Pubkey),
    gsig: &[&[&[u8]]],
) -> Result<(u64, u64, u64)> {
    let cpmm = a.config.cpmm_program;
    let pool = a.launch.pool;
    let grad = a.grad_auth.key();
    let lp_mint = raydium::lp_mint(&cpmm, &pool);
    let launch_key = a.launch.key();
    let system = a.system_program.to_account_info();
    let token_prog = a.token_program.to_account_info();
    let (lp_supply, f0, f1) = {
        let data = a.pool_state.try_borrow_data()?;
        raydium::pool_lp_and_fees(&data).ok_or(FairError::WrongPoolAccount)?
    };
    let r0 = token_amount(&a.token_0_vault.to_account_info())?.saturating_sub(f0);
    let r1 = token_amount(&a.token_1_vault.to_account_info())?.saturating_sub(f1);
    // token_is_1: the coin is token_1, so WSOL is token_0.
    let (sol_res, tok_res) = if token_is_1 { (r0, r1) } else { (r1, r0) };
    let (lp, sol_used, tok_used) = math::lp_for_deposit(budget, tokens, sol_res, tok_res, lp_supply);
    if lp > 0 {
        require_keys_eq!(a.grad_lp.key(), associated_token::get_associated_token_address(&grad, &lp_mint), FairError::WrongPoolAccount);
        associated_token::create_idempotent(CpiContext::new(
            a.associated_token_program.to_account_info(),
            associated_token::Create {
                payer: a.payer.to_account_info(),
                associated_token: a.grad_lp.to_account_info(),
                authority: a.grad_auth.to_account_info(),
                mint: a.lp_mint.to_account_info(),
                system_program: system.clone(),
                token_program: token_prog.clone(),
            },
        ))?;
        let (max0, max1) = if token_is_1 { (sol_used, tok_used) } else { (tok_used, sol_used) };
        let dk = raydium::DepositKeys {
            program: cpmm,
            owner: grad,
            authority: raydium::authority(&cpmm),
            pool_state: pool,
            owner_lp_token: a.grad_lp.key(),
            token_0_account: r0_key,
            token_1_account: r1_key,
            token_0_vault: a.token_0_vault.key(),
            token_1_vault: a.token_1_vault.key(),
            token_program: token::ID,
            token_program_2022: raydium::lock::TOKEN_2022,
            vault_0_mint: mint0,
            vault_1_mint: mint1,
            lp_mint,
        };
        let (m0d, m1d) = if token_is_1 { (a.wsol_mint.to_account_info(), a.mint.to_account_info()) } else { (a.mint.to_account_info(), a.wsol_mint.to_account_info()) };
        let (c0d, c1d) = if token_is_1 { (a.grad_wsol.to_account_info(), a.grad_token.to_account_info()) } else { (a.grad_token.to_account_info(), a.grad_wsol.to_account_info()) };
        invoke_signed(
            &raydium::deposit_ix(&dk, lp, max0, max1),
            &[
                a.grad_auth.to_account_info(), a.ray_authority.to_account_info(), a.pool_state.to_account_info(),
                a.grad_lp.to_account_info(), c0d, c1d, a.token_0_vault.to_account_info(), a.token_1_vault.to_account_info(),
                token_prog.clone(), a.token_2022_program.to_account_info(), m0d, m1d, a.lp_mint.to_account_info(),
                a.cpmm_program.to_account_info(),
            ],
            gsig,
        )?;
        token::burn(
            CpiContext::new_with_signer(
                token_prog.clone(),
                token::Burn { mint: a.lp_mint.to_account_info(), from: a.grad_lp.to_account_info(), authority: a.grad_auth.to_account_info() },
                gsig,
            ),
            lp,
        )?;
        emit!(LiquidityRecycled { launch: launch_key, sol: sol_used, tokens: tok_used, lp_burned: lp });
    }
    Ok((lp, sol_used, tok_used))
}

// ───────────────────────────── accounts ─────────────────────────────

#[derive(Accounts)]
pub struct InitConfig<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(init, payer = authority, space = 8 + Config::INIT_SPACE, seeds = [CONFIG_SEED], bump)]
    pub config: Box<Account<'info, Config>>,
    /// Only the program's upgrade authority can initialise the config, so nobody
    /// can grab a freshly deployed program first.
    #[account(constraint = program.programdata_address()? == Some(program_data.key()) @ FairError::InvalidConfig)]
    pub program: Program<'info, crate::program::Fairlaunch>,
    #[account(constraint = program_data.upgrade_authority_address == Some(authority.key()) @ FairError::InvalidConfig)]
    pub program_data: Account<'info, ProgramData>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct AdminConfig<'info> {
    pub authority: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump, has_one = authority)]
    pub config: Box<Account<'info, Config>>,
}

#[derive(Accounts)]
pub struct ApplyConfig<'info> {
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
}

#[derive(Accounts)]
pub struct AcceptAuthority<'info> {
    pub new_authority: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
}

#[derive(Accounts)]
#[instruction(seed: [u8; SEED_LEN])]
pub struct CreateLaunch<'info> {
    #[account(mut)]
    pub creator: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    /// CHECK: validated against config.treasury
    #[account(mut)]
    pub treasury: UncheckedAccount<'info>,
    #[account(init, payer = creator, seeds = [MINT_SEED, seed.as_ref()], bump, mint::decimals = DECIMALS, mint::authority = launch)]
    pub mint: Box<Account<'info, Mint>>,
    #[account(init, payer = creator, space = 8 + Launch::INIT_SPACE, seeds = [LAUNCH_SEED, mint.key().as_ref()], bump)]
    pub launch: Box<Account<'info, Launch>>,
    #[account(init, payer = creator, associated_token::mint = mint, associated_token::authority = launch)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(init, payer = creator, seeds = [ESCROW_SEED, launch.key().as_ref()], bump, token::mint = mint, token::authority = launch)]
    pub escrow: Box<Account<'info, TokenAccount>>,
    /// CHECK: created by Metaplex (it checks the PDA)
    #[account(mut)]
    pub metadata: UncheckedAccount<'info>,
    pub metadata_program: Program<'info, Metadata>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
    /// CHECK: checked against config.storage_wallet when storage is on
    #[account(mut)]
    pub storage: UncheckedAccount<'info>,
}

#[derive(Accounts)]
#[instruction(seed: [u8; SEED_LEN])]
pub struct CreateLaunchSigned<'info> {
    /// Pays the rent for the new coin's accounts (normally its first buyer).
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(init, payer = payer, seeds = [MINT_SEED, seed.as_ref()], bump, mint::decimals = DECIMALS, mint::authority = launch)]
    pub mint: Box<Account<'info, Mint>>,
    #[account(init, payer = payer, space = 8 + Launch::INIT_SPACE, seeds = [LAUNCH_SEED, mint.key().as_ref()], bump)]
    pub launch: Box<Account<'info, Launch>>,
    #[account(init, payer = payer, associated_token::mint = mint, associated_token::authority = launch)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(init, payer = payer, seeds = [ESCROW_SEED, launch.key().as_ref()], bump, token::mint = mint, token::authority = launch)]
    pub escrow: Box<Account<'info, TokenAccount>>,
    /// CHECK: created by Metaplex (it checks the PDA)
    #[account(mut)]
    pub metadata: UncheckedAccount<'info>,
    /// CHECK: the instructions sysvar, read to find the creator's Ed25519 approval
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions: UncheckedAccount<'info>,
    pub metadata_program: Program<'info, Metadata>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
    /// CHECK: checked against config.storage_wallet when storage is on
    #[account(mut)]
    pub storage: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct Commit<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [LAUNCH_SEED, launch.mint.as_ref()], bump = launch.bump)]
    pub launch: Box<Account<'info, Launch>>,
    #[account(init_if_needed, payer = user, space = 8 + Commitment::INIT_SPACE, seeds = [COMMIT_SEED, launch.key().as_ref(), user.key().as_ref()], bump)]
    pub commitment: Box<Account<'info, Commitment>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Settle<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    /// CHECK: validated against config.treasury
    #[account(mut)]
    pub treasury: UncheckedAccount<'info>,
    #[account(mut, seeds = [LAUNCH_SEED, launch.mint.as_ref()], bump = launch.bump)]
    pub launch: Box<Account<'info, Launch>>,
    #[account(seeds = [CONFIG_EXT_SEED], bump = config_ext.bump)]
    pub config_ext: Box<Account<'info, ConfigExt>>,
}

#[derive(Accounts)]
pub struct InitConfigExt<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = authority)]
    pub config: Box<Account<'info, Config>>,
    #[account(init, payer = authority, space = 8 + ConfigExt::INIT_SPACE, seeds = [CONFIG_EXT_SEED], bump)]
    pub config_ext: Box<Account<'info, ConfigExt>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct AdminConfigExt<'info> {
    pub authority: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = authority)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [CONFIG_EXT_SEED], bump = config_ext.bump)]
    pub config_ext: Box<Account<'info, ConfigExt>>,
}

#[derive(Accounts)]
pub struct ApplyConfigExt<'info> {
    #[account(mut, seeds = [CONFIG_EXT_SEED], bump = config_ext.bump)]
    pub config_ext: Box<Account<'info, ConfigExt>>,
}

#[derive(Accounts)]
pub struct Claim<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(mut, seeds = [LAUNCH_SEED, mint.key().as_ref()], bump = launch.bump, has_one = mint)]
    pub launch: Box<Account<'info, Launch>>,
    pub mint: Box<Account<'info, Mint>>,
    /// CHECK: read by hand so commitments from before partial claims still work; the seeds pin it
    /// to this launch and user, and the handler checks owner, discriminator and fields.
    #[account(mut, seeds = [COMMIT_SEED, launch.key().as_ref(), user.key().as_ref()], bump)]
    pub commitment: UncheckedAccount<'info>,
    #[account(mut, associated_token::mint = mint, associated_token::authority = launch)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(init_if_needed, payer = user, associated_token::mint = mint, associated_token::authority = user)]
    pub user_ata: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    #[account(init_if_needed, payer = user, space = 8 + Pass::INIT_SPACE, seeds = [PASS_SEED, user.key().as_ref()], bump)]
    pub pass: Box<Account<'info, Pass>>,
}

#[derive(Accounts)]
pub struct CreatorEscrow<'info> {
    #[account(mut)]
    pub creator: Signer<'info>,
    #[account(mut, seeds = [LAUNCH_SEED, mint.key().as_ref()], bump = launch.bump, has_one = mint, has_one = creator)]
    pub launch: Box<Account<'info, Launch>>,
    pub mint: Box<Account<'info, Mint>>,
    #[account(mut, seeds = [ESCROW_SEED, launch.key().as_ref()], bump)]
    pub escrow: Box<Account<'info, TokenAccount>>,
    #[account(mut, associated_token::mint = mint, associated_token::authority = launch)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(init_if_needed, payer = creator, associated_token::mint = mint, associated_token::authority = creator)]
    pub creator_ata: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct TradeCtx<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    /// CHECK: validated against config.treasury
    #[account(mut)]
    pub treasury: UncheckedAccount<'info>,
    #[account(mut, seeds = [LAUNCH_SEED, mint.key().as_ref()], bump = launch.bump, has_one = mint)]
    pub launch: Box<Account<'info, Launch>>,
    pub mint: Box<Account<'info, Mint>>,
    #[account(mut, associated_token::mint = mint, associated_token::authority = launch)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(init_if_needed, payer = user, associated_token::mint = mint, associated_token::authority = user)]
    pub user_ata: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    /// CHECK: the user's line commitment for this coin (usually empty). Its address is fixed by
    /// the seeds; buys count any unclaimed line tokens in it toward the 3% cap.
    #[account(seeds = [COMMIT_SEED, launch.key().as_ref(), user.key().as_ref()], bump)]
    pub commitment: UncheckedAccount<'info>,
    #[account(init_if_needed, payer = user, space = 8 + Pass::INIT_SPACE, seeds = [PASS_SEED, user.key().as_ref()], bump)]
    pub pass: Box<Account<'info, Pass>>,
}

#[derive(Accounts)]
pub struct Distribute<'info> {
    pub distributor: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [LAUNCH_SEED, mint.key().as_ref()], bump = launch.bump, has_one = mint)]
    pub launch: Box<Account<'info, Launch>>,
    pub mint: Box<Account<'info, Mint>>,
    #[account(associated_token::mint = mint, associated_token::authority = launch)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(seeds = [ESCROW_SEED, launch.key().as_ref()], bump)]
    pub escrow: Box<Account<'info, TokenAccount>>,
    /// CHECK: the pool's token vault after graduation (checked in the handler); None before.
    pub pool_token_vault: Option<UncheckedAccount<'info>>,
}

#[derive(Accounts)]
pub struct PrepareGraduation<'info> {
    pub payer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    /// CHECK: validated against config.treasury
    #[account(mut)]
    pub treasury: UncheckedAccount<'info>,
    #[account(mut, seeds = [LAUNCH_SEED, launch.mint.as_ref()], bump = launch.bump)]
    pub launch: Box<Account<'info, Launch>>,
    /// CHECK: graduation authority PDA (plain system account)
    #[account(mut, seeds = [GRAD_SEED, launch.key().as_ref()], bump = launch.grad_bump)]
    pub grad_auth: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct Graduate<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    /// CHECK: validated against config.treasury
    #[account(mut)]
    pub treasury: UncheckedAccount<'info>,
    #[account(mut, seeds = [LAUNCH_SEED, mint.key().as_ref()], bump = launch.bump, has_one = mint)]
    pub launch: Box<Account<'info, Launch>>,
    pub mint: Box<Account<'info, Mint>>,
    #[account(mut, associated_token::mint = mint, associated_token::authority = launch)]
    pub vault: Box<Account<'info, TokenAccount>>,
    /// CHECK: system-owned PDA that creates the pool and owns its fee accounts
    #[account(mut, seeds = [GRAD_SEED, launch.key().as_ref()], bump = launch.grad_bump)]
    pub grad_auth: UncheckedAccount<'info>,
    /// CHECK: ATA(grad_auth, WSOL), checked in the handler
    #[account(mut)]
    pub grad_wsol: UncheckedAccount<'info>,
    /// CHECK: ATA(grad_auth, mint), checked in the handler
    #[account(mut)]
    pub grad_token: UncheckedAccount<'info>,
    /// CHECK: ATA(grad_auth, lp_mint), created by Raydium, checked in the handler
    #[account(mut)]
    pub grad_lp: UncheckedAccount<'info>,
    /// CHECK: native mint
    #[account(address = spl_token::native_mint::ID)]
    pub wsol_mint: UncheckedAccount<'info>,
    /// CHECK: must match config
    #[account(executable, address = config.cpmm_program)]
    pub cpmm_program: UncheckedAccount<'info>,
    /// CHECK: must match config
    #[account(address = config.amm_config)]
    pub amm_config: UncheckedAccount<'info>,
    /// CHECK: Raydium authority PDA, checked in the handler
    pub ray_authority: UncheckedAccount<'info>,
    /// Fresh keypair for the Raydium pool account (signs; Raydium creates it).
    #[account(mut)]
    pub pool_state: Signer<'info>,
    /// CHECK: Raydium LP mint PDA, checked in the handler
    #[account(mut)]
    pub lp_mint: UncheckedAccount<'info>,
    /// CHECK: Raydium vault PDA, checked in the handler
    #[account(mut)]
    pub token_0_vault: UncheckedAccount<'info>,
    /// CHECK: Raydium vault PDA, checked in the handler
    #[account(mut)]
    pub token_1_vault: UncheckedAccount<'info>,
    /// CHECK: Raydium's fee receiver, checked by Raydium
    #[account(mut)]
    pub create_pool_fee: UncheckedAccount<'info>,
    /// CHECK: Raydium observation PDA, checked in the handler
    #[account(mut)]
    pub observation: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct LockLiquidity<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [LAUNCH_SEED, launch.mint.as_ref()], bump = launch.bump)]
    pub launch: Box<Account<'info, Launch>>,
    /// CHECK: graduation authority PDA
    #[account(mut, seeds = [GRAD_SEED, launch.key().as_ref()], bump = launch.grad_bump)]
    pub grad_auth: UncheckedAccount<'info>,
    /// CHECK: checked in the handler
    #[account(mut)]
    pub grad_lp: UncheckedAccount<'info>,
    /// New keypair for the fee-key NFT mint (signs the transaction).
    #[account(mut)]
    pub fee_nft_mint: Signer<'info>,
    /// CHECK: ATA(grad_auth, fee_nft_mint), created by the locker
    #[account(mut)]
    pub fee_nft_account: UncheckedAccount<'info>,
    /// CHECK: the NFT's metadata PDA (no metadata is written)
    #[account(mut)]
    pub nft_metadata: UncheckedAccount<'info>,
    /// CHECK: must match config
    #[account(executable, address = config.lock_program)]
    pub lock_program: UncheckedAccount<'info>,
    /// CHECK: checked in the handler
    pub lock_authority: UncheckedAccount<'info>,
    /// CHECK: checked in the handler
    #[account(mut)]
    pub locked_liquidity: UncheckedAccount<'info>,
    /// CHECK: checked in the handler
    #[account(mut)]
    pub locked_lp_vault: UncheckedAccount<'info>,
    /// CHECK: must equal launch.pool
    pub pool_state: UncheckedAccount<'info>,
    /// CHECK: checked in the handler
    #[account(mut)]
    pub lp_mint: UncheckedAccount<'info>,
    /// CHECK: checked in the handler
    #[account(mut)]
    pub token_0_vault: UncheckedAccount<'info>,
    /// CHECK: checked in the handler
    #[account(mut)]
    pub token_1_vault: UncheckedAccount<'info>,
    pub metadata_program: Program<'info, Metadata>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct CollectPoolFees<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    /// CHECK: validated against config.treasury
    #[account(mut)]
    pub treasury: UncheckedAccount<'info>,
    #[account(mut, seeds = [LAUNCH_SEED, mint.key().as_ref()], bump = launch.bump, has_one = mint)]
    pub launch: Box<Account<'info, Launch>>,
    /// CHECK: must be launch.creator, receives the creator share
    #[account(mut)]
    pub creator: UncheckedAccount<'info>,
    #[account(mut)]
    pub mint: Box<Account<'info, Mint>>,
    /// CHECK: graduation authority PDA
    #[account(mut, seeds = [GRAD_SEED, launch.key().as_ref()], bump = launch.grad_bump)]
    pub grad_auth: UncheckedAccount<'info>,
    /// CHECK: checked in the handler
    #[account(mut)]
    pub grad_wsol: UncheckedAccount<'info>,
    /// CHECK: checked in the handler
    #[account(mut)]
    pub grad_token: UncheckedAccount<'info>,
    /// CHECK: native mint
    #[account(address = spl_token::native_mint::ID)]
    pub wsol_mint: UncheckedAccount<'info>,
    /// CHECK: must match config
    #[account(executable, address = config.cpmm_program)]
    pub cpmm_program: UncheckedAccount<'info>,
    /// CHECK: Raydium authority; the locker checks it
    pub ray_authority: UncheckedAccount<'info>,
    /// CHECK: must equal launch.pool
    #[account(mut)]
    pub pool_state: UncheckedAccount<'info>,
    /// CHECK: checked in the handler
    #[account(mut)]
    pub lp_mint: UncheckedAccount<'info>,
    /// CHECK: checked in the handler
    #[account(mut)]
    pub token_0_vault: UncheckedAccount<'info>,
    /// CHECK: checked in the handler
    #[account(mut)]
    pub token_1_vault: UncheckedAccount<'info>,
    /// CHECK: must match config
    #[account(executable, address = config.lock_program)]
    pub lock_program: UncheckedAccount<'info>,
    /// CHECK: checked in the handler
    pub lock_authority: UncheckedAccount<'info>,
    /// CHECK: checked in the handler
    #[account(mut)]
    pub locked_liquidity: UncheckedAccount<'info>,
    /// CHECK: checked in the handler
    pub fee_nft_account: UncheckedAccount<'info>,
    /// CHECK: checked in the handler
    #[account(mut)]
    pub locked_lp_vault: UncheckedAccount<'info>,
    /// CHECK: Token-2022 program id
    #[account(address = raydium::lock::TOKEN_2022)]
    pub token_2022_program: UncheckedAccount<'info>,
    /// CHECK: memo program id
    #[account(address = raydium::lock::MEMO_PROGRAM)]
    pub memo_program: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    /// CHECK: ATA(grad_auth, lp_mint), checked in the handler; receives the recycled
    /// liquidity's LP tokens, which are burned straight away
    #[account(mut)]
    pub grad_lp: UncheckedAccount<'info>,
}
