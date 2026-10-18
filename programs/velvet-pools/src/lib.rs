//! Velvet Pools: the constant-product pools coins move into when they bond on Velvet Launch.
//!
//! Derived from raydium-io/raydium-cp-swap (Apache-2.0), commit 0f86d46. Modified by Velvet:
//! - one pair per pool: SOL (wrapped) and the coin, both classic SPL tokens (no Token-2022);
//! - only Velvet Launch can create a pool: the coin's graduation authority (a PDA of the
//!   launchpad) must sign, so nobody else can create or front-run a coin's pool;
//! - no LP tokens, no deposit or withdraw: the liquidity a coin bonds with is locked forever;
//! - one fixed fee, always charged in SOL: 0.35% per swap, of which 0.20% stays in the pool
//!   and 0.15% is set aside for the coin's holders, its creator and Velvet, collected through
//!   the launchpad;
//! - no admin, no config, no pause.
//! The curve step and fee rounding are Raydium's (see curve/).

use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer};

pub mod curve;
use curve::calculator::CurveCalculator;

declare_id!("VPkPeiAg7LheTtDdBByCmXewabF9zF6yK5YYLmfPjWb");

/// Velvet Launch, the only program whose graduation authority can create pools.
pub mod launchpad {
    use anchor_lang::prelude::declare_id;
    declare_id!("8f9XtMzn9J8vapxoLoypKUjNta64jhjfr18WMVCn5yAk");
}

pub const POOL_SEED: &[u8] = b"pool";
pub const VAULT_SEED: &[u8] = b"vault";
pub const AUTH_SEED: &[u8] = b"vault_auth";
/// The launchpad's per-coin graduation authority: ["grad", launch].
pub const GRAD_SEED: &[u8] = b"grad";

/// 0.35% of the SOL side of every swap.
pub const TRADE_FEE_RATE: u64 = 3_500;
/// Share of that fee set aside for holders, creator and Velvet (0.15 of 0.35), per million.
pub const PROTOCOL_FEE_RATE: u64 = 428_571;

#[program]
pub mod velvet_pools {
    use super::*;

    /// Create a coin's pool with the liquidity it bonded with. Only the coin's graduation
    /// authority on Velvet Launch can call this (it signs through the launchpad).
    pub fn initialize(ctx: Context<Initialize>, launch: Pubkey, grad_bump: u8, sol_amount: u64, token_amount: u64) -> Result<()> {
        let expected = Pubkey::create_program_address(&[GRAD_SEED, launch.as_ref(), &[grad_bump]], &launchpad::ID)
            .map_err(|_| error!(PoolError::NotLaunchpad))?;
        require_keys_eq!(ctx.accounts.creator.key(), expected, PoolError::NotLaunchpad);
        require!(sol_amount > 0 && token_amount > 0, PoolError::EmptySupply);
        require_keys_eq!(ctx.accounts.wsol_mint.key(), token::spl_token::native_mint::ID, PoolError::WrongMint);
        require_keys_neq!(ctx.accounts.token_mint.key(), ctx.accounts.wsol_mint.key(), PoolError::WrongMint);

        let tp = ctx.accounts.token_program.to_account_info();
        token::transfer(
            CpiContext::new(tp.clone(), Transfer { from: ctx.accounts.creator_sol.to_account_info(), to: ctx.accounts.sol_vault.to_account_info(), authority: ctx.accounts.creator.to_account_info() }),
            sol_amount,
        )?;
        token::transfer(
            CpiContext::new(tp, Transfer { from: ctx.accounts.creator_token.to_account_info(), to: ctx.accounts.token_vault.to_account_info(), authority: ctx.accounts.creator.to_account_info() }),
            token_amount,
        )?;

        let p = &mut ctx.accounts.pool;
        p.launch = launch;
        p.owner = expected;
        p.token_mint = ctx.accounts.token_mint.key();
        p.sol_vault = ctx.accounts.sol_vault.key();
        p.token_vault = ctx.accounts.token_vault.key();
        p.bump = ctx.bumps.pool;
        p.auth_bump = ctx.bumps.vault_auth;
        p.created_at = Clock::get()?.unix_timestamp;
        emit!(PoolCreated { pool: p.key(), launch, token_mint: p.token_mint, sol_amount, token_amount });
        Ok(())
    }

    /// Swap an exact input: `buy` = SOL in, coin out; otherwise coin in, SOL out.
    /// Fails instead of returning less than `minimum_amount_out`.
    pub fn swap(ctx: Context<Swap>, buy: bool, amount_in: u64, minimum_amount_out: u64) -> Result<()> {
        let a = &ctx.accounts;
        let p = &a.pool;
        let sol = u128::from(a.sol_vault.amount).checked_sub(u128::from(p.protocol_fees_sol)).ok_or(PoolError::Math)?;
        let tok = u128::from(a.token_vault.amount);
        let k_before = sol.checked_mul(tok).ok_or(PoolError::Math)?;
        let r = if buy {
            CurveCalculator::buy(amount_in, sol, tok, TRADE_FEE_RATE, PROTOCOL_FEE_RATE)
        } else {
            CurveCalculator::sell(amount_in, sol, tok, TRADE_FEE_RATE, PROTOCOL_FEE_RATE)
        }
        .ok_or(PoolError::ZeroTradingTokens)?;
        require!(r.new_sol.checked_mul(r.new_token).ok_or(PoolError::Math)? >= k_before, PoolError::Math);
        require!(r.amount_out >= minimum_amount_out, PoolError::ExceededSlippage);

        let tp = a.token_program.to_account_info();
        let auth_seeds: &[&[u8]] = &[AUTH_SEED, &[p.auth_bump]];
        let signer = &[auth_seeds];
        let (user_in, vault_in, vault_out, user_out) = if buy {
            (a.user_sol.to_account_info(), a.sol_vault.to_account_info(), a.token_vault.to_account_info(), a.user_token.to_account_info())
        } else {
            (a.user_token.to_account_info(), a.token_vault.to_account_info(), a.sol_vault.to_account_info(), a.user_sol.to_account_info())
        };
        token::transfer(CpiContext::new(tp.clone(), Transfer { from: user_in, to: vault_in, authority: a.user.to_account_info() }), amount_in)?;
        token::transfer(CpiContext::new_with_signer(tp, Transfer { from: vault_out, to: user_out, authority: a.vault_auth.to_account_info() }, signer), r.amount_out)?;

        let p = &mut ctx.accounts.pool;
        p.protocol_fees_sol = p.protocol_fees_sol.checked_add(r.protocol_fee).ok_or(PoolError::Math)?;
        let sol_amount = if buy { amount_in } else { r.amount_out };
        p.volume_sol = p.volume_sol.saturating_add(sol_amount);
        p.swaps = p.swaps.saturating_add(1);
        emit!(SwapEvent {
            pool: p.key(), user: ctx.accounts.user.key(), buy,
            sol_amount, token_amount: if buy { r.amount_out } else { amount_in },
            fee: r.trade_fee, protocol_fee: r.protocol_fee,
            sol_reserve: u64::try_from(r.new_sol).map_err(|_| PoolError::Math)?, token_reserve: u64::try_from(r.new_token).map_err(|_| PoolError::Math)?,
        });
        Ok(())
    }

    /// Hand the fees set aside so far (WSOL) to the pool's owner (the coin's graduation
    /// authority), which splits them between holders, creator and Velvet.
    pub fn collect_fees(ctx: Context<CollectFees>) -> Result<()> {
        let p = &ctx.accounts.pool;
        require_keys_eq!(ctx.accounts.owner.key(), p.owner, PoolError::NotOwner);
        let amount = p.protocol_fees_sol.min(ctx.accounts.sol_vault.amount);
        if amount > 0 {
            let auth_seeds: &[&[u8]] = &[AUTH_SEED, &[p.auth_bump]];
            token::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer { from: ctx.accounts.sol_vault.to_account_info(), to: ctx.accounts.destination.to_account_info(), authority: ctx.accounts.vault_auth.to_account_info() },
                    &[auth_seeds],
                ),
                amount,
            )?;
        }
        let p = &mut ctx.accounts.pool;
        p.protocol_fees_sol -= amount;
        p.fees_collected = p.fees_collected.saturating_add(amount);
        emit!(FeesCollected { pool: p.key(), amount });
        Ok(())
    }
}

#[account]
#[derive(InitSpace)]
pub struct Pool {
    pub launch: Pubkey,
    /// The coin's graduation authority on Velvet Launch: the only key that can collect fees.
    pub owner: Pubkey,
    pub token_mint: Pubkey,
    pub sol_vault: Pubkey,
    pub token_vault: Pubkey,
    pub bump: u8,
    pub auth_bump: u8,
    /// SOL in the vault that belongs to holders, creator and Velvet (not to the pool).
    pub protocol_fees_sol: u64,
    pub fees_collected: u64,
    pub volume_sol: u64,
    pub swaps: u64,
    pub created_at: i64,
    pub _reserved: [u8; 64],
}

#[derive(Accounts)]
#[instruction(launch: Pubkey)]
pub struct Initialize<'info> {
    /// The coin's graduation authority (a launchpad PDA, checked in the handler); pays the rent.
    #[account(mut)]
    pub creator: Signer<'info>,
    pub token_mint: Box<Account<'info, Mint>>,
    pub wsol_mint: Box<Account<'info, Mint>>,
    #[account(init, payer = creator, space = 8 + Pool::INIT_SPACE, seeds = [POOL_SEED, token_mint.key().as_ref()], bump)]
    pub pool: Box<Account<'info, Pool>>,
    /// CHECK: signs for the vaults; only a PDA of this program
    #[account(seeds = [AUTH_SEED], bump)]
    pub vault_auth: UncheckedAccount<'info>,
    #[account(init, payer = creator, seeds = [VAULT_SEED, pool.key().as_ref(), wsol_mint.key().as_ref()], bump, token::mint = wsol_mint, token::authority = vault_auth)]
    pub sol_vault: Box<Account<'info, TokenAccount>>,
    #[account(init, payer = creator, seeds = [VAULT_SEED, pool.key().as_ref(), token_mint.key().as_ref()], bump, token::mint = token_mint, token::authority = vault_auth)]
    pub token_vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = wsol_mint, token::authority = creator)]
    pub creator_sol: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = token_mint, token::authority = creator)]
    pub creator_token: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Swap<'info> {
    pub user: Signer<'info>,
    #[account(mut, seeds = [POOL_SEED, pool.token_mint.as_ref()], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    /// CHECK: signs for the vaults; only a PDA of this program
    #[account(seeds = [AUTH_SEED], bump = pool.auth_bump)]
    pub vault_auth: UncheckedAccount<'info>,
    #[account(mut, address = pool.sol_vault)]
    pub sol_vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, address = pool.token_vault)]
    pub token_vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = token::spl_token::native_mint::ID, token::authority = user)]
    pub user_sol: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = pool.token_mint, token::authority = user)]
    pub user_token: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct CollectFees<'info> {
    pub owner: Signer<'info>,
    #[account(mut, seeds = [POOL_SEED, pool.token_mint.as_ref()], bump = pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    /// CHECK: signs for the vaults; only a PDA of this program
    #[account(seeds = [AUTH_SEED], bump = pool.auth_bump)]
    pub vault_auth: UncheckedAccount<'info>,
    #[account(mut, address = pool.sol_vault)]
    pub sol_vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = token::spl_token::native_mint::ID)]
    pub destination: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
}

#[event]
pub struct PoolCreated {
    pub pool: Pubkey,
    pub launch: Pubkey,
    pub token_mint: Pubkey,
    pub sol_amount: u64,
    pub token_amount: u64,
}

#[event]
pub struct SwapEvent {
    pub pool: Pubkey,
    pub user: Pubkey,
    pub buy: bool,
    /// SOL paid (buy, fee included) or received (sell, after fee).
    pub sol_amount: u64,
    pub token_amount: u64,
    pub fee: u64,
    pub protocol_fee: u64,
    pub sol_reserve: u64,
    pub token_reserve: u64,
}

#[event]
pub struct FeesCollected {
    pub pool: Pubkey,
    pub amount: u64,
}

#[error_code]
pub enum PoolError {
    #[msg("Only Velvet Launch can create pools")]
    NotLaunchpad,
    #[msg("Pool needs both SOL and tokens")]
    EmptySupply,
    #[msg("Wrong mint")]
    WrongMint,
    #[msg("Trade too small or too big for this pool")]
    ZeroTradingTokens,
    #[msg("The price moved more than your slippage limit")]
    ExceededSlippage,
    #[msg("Only the pool's owner can collect its fees")]
    NotOwner,
    #[msg("Math error")]
    Math,
}
