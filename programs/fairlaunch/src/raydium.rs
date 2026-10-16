//! Hand-written CPI into Raydium CPMM (raydium-io/raydium-cp-swap).
//! We build the instructions directly instead of depending on their crate, which
//! targets a newer Anchor than this program.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};

pub const IX_INITIALIZE: [u8; 8] = [175, 175, 109, 31, 13, 152, 155, 237];
pub const IX_COLLECT_CREATOR_FEE: [u8; 8] = [20, 22, 86, 123, 198, 28, 219, 132];

pub const AUTH_SEED: &[u8] = b"vault_and_lp_mint_auth_seed";
pub const POOL_SEED: &[u8] = b"pool";
pub const POOL_LP_MINT_SEED: &[u8] = b"pool_lp_mint";
pub const POOL_VAULT_SEED: &[u8] = b"pool_vault";
pub const OBSERVATION_SEED: &[u8] = b"observation";
pub const CREATOR_FEE_SHARE_SEED: &[u8] = b"creator_fee_share";

pub fn authority(program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[AUTH_SEED], program).0
}
pub fn pool_state(program: &Pubkey, amm_config: &Pubkey, mint0: &Pubkey, mint1: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[POOL_SEED, amm_config.as_ref(), mint0.as_ref(), mint1.as_ref()], program).0
}
pub fn lp_mint(program: &Pubkey, pool: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[POOL_LP_MINT_SEED, pool.as_ref()], program).0
}
pub fn vault(program: &Pubkey, pool: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[POOL_VAULT_SEED, pool.as_ref(), mint.as_ref()], program).0
}
pub fn observation(program: &Pubkey, pool: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[OBSERVATION_SEED, pool.as_ref()], program).0
}
pub fn creator_fee_share(program: &Pubkey, creator: &Pubkey, amm_config: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[CREATOR_FEE_SHARE_SEED, creator.as_ref(), amm_config.as_ref()], program).0
}

/// Sort two mints the way Raydium requires (token_0 < token_1).
pub fn sorted(a: Pubkey, b: Pubkey) -> (Pubkey, Pubkey, bool) {
    if a < b { (a, b, false) } else { (b, a, true) }
}

pub struct InitializeKeys {
    pub program: Pubkey,
    pub creator: Pubkey,
    pub amm_config: Pubkey,
    pub authority: Pubkey,
    pub pool_state: Pubkey,
    pub token_0_mint: Pubkey,
    pub token_1_mint: Pubkey,
    pub lp_mint: Pubkey,
    pub creator_token_0: Pubkey,
    pub creator_token_1: Pubkey,
    pub creator_lp_token: Pubkey,
    pub token_0_vault: Pubkey,
    pub token_1_vault: Pubkey,
    pub create_pool_fee: Pubkey,
    pub observation_state: Pubkey,
    pub token_program: Pubkey,
    pub associated_token_program: Pubkey,
    pub system_program: Pubkey,
    pub rent: Pubkey,
}

pub fn initialize_ix(k: &InitializeKeys, amount_0: u64, amount_1: u64, open_time: u64) -> Instruction {
    let mut data = IX_INITIALIZE.to_vec();
    data.extend_from_slice(&amount_0.to_le_bytes());
    data.extend_from_slice(&amount_1.to_le_bytes());
    data.extend_from_slice(&open_time.to_le_bytes());
    Instruction {
        program_id: k.program,
        accounts: vec![
            AccountMeta::new(k.creator, true),
            AccountMeta::new_readonly(k.amm_config, false),
            AccountMeta::new_readonly(k.authority, false),
            AccountMeta::new(k.pool_state, true),
            AccountMeta::new_readonly(k.token_0_mint, false),
            AccountMeta::new_readonly(k.token_1_mint, false),
            AccountMeta::new(k.lp_mint, false),
            AccountMeta::new(k.creator_token_0, false),
            AccountMeta::new(k.creator_token_1, false),
            AccountMeta::new(k.creator_lp_token, false),
            AccountMeta::new(k.token_0_vault, false),
            AccountMeta::new(k.token_1_vault, false),
            AccountMeta::new(k.create_pool_fee, false),
            AccountMeta::new(k.observation_state, false),
            AccountMeta::new_readonly(k.token_program, false),
            AccountMeta::new_readonly(k.token_program, false), // token_0_program
            AccountMeta::new_readonly(k.token_program, false), // token_1_program
            AccountMeta::new_readonly(k.associated_token_program, false),
            AccountMeta::new_readonly(k.system_program, false),
            AccountMeta::new_readonly(k.rent, false),
        ],
        data,
    }
}

pub struct CollectKeys {
    pub program: Pubkey,
    pub creator: Pubkey,
    pub authority: Pubkey,
    pub pool_state: Pubkey,
    pub amm_config: Pubkey,
    pub token_0_vault: Pubkey,
    pub token_1_vault: Pubkey,
    pub vault_0_mint: Pubkey,
    pub vault_1_mint: Pubkey,
    pub creator_token_0: Pubkey,
    pub creator_token_1: Pubkey,
    pub token_program: Pubkey,
    pub associated_token_program: Pubkey,
    pub system_program: Pubkey,
    pub creator_fee_share: Pubkey,
}

pub fn collect_creator_fee_ix(k: &CollectKeys) -> Instruction {
    Instruction {
        program_id: k.program,
        accounts: vec![
            AccountMeta::new(k.creator, true),
            AccountMeta::new_readonly(k.authority, false),
            AccountMeta::new(k.pool_state, false),
            AccountMeta::new_readonly(k.amm_config, false),
            AccountMeta::new(k.token_0_vault, false),
            AccountMeta::new(k.token_1_vault, false),
            AccountMeta::new_readonly(k.vault_0_mint, false),
            AccountMeta::new_readonly(k.vault_1_mint, false),
            AccountMeta::new(k.creator_token_0, false),
            AccountMeta::new(k.creator_token_1, false),
            AccountMeta::new_readonly(k.token_program, false),
            AccountMeta::new_readonly(k.token_program, false),
            AccountMeta::new_readonly(k.associated_token_program, false),
            AccountMeta::new_readonly(k.system_program, false),
            AccountMeta::new_readonly(k.creator_fee_share, false),
        ],
        data: IX_COLLECT_CREATOR_FEE.to_vec(),
    }
}

// ── Raydium liquidity locker ("Burn & Earn") ──
pub mod lock {
    use super::*;
    pub const IX_LOCK_CP_LIQUIDITY: [u8; 8] = [216, 157, 29, 78, 38, 51, 31, 26];
    pub const IX_COLLECT_CP_FEES: [u8; 8] = [8, 30, 51, 199, 209, 184, 247, 133];
    pub const LOCK_CP_AUTH_SEED: &[u8] = b"lock_cp_authority_seed";
    pub const LOCKED_LIQUIDITY_SEED: &[u8] = b"locked_liquidity";
    pub const MEMO_PROGRAM: Pubkey = anchor_lang::solana_program::pubkey!("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr");
    pub const TOKEN_2022: Pubkey = anchor_lang::solana_program::pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

    pub fn authority(lock_program: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[LOCK_CP_AUTH_SEED], lock_program).0
    }
    pub fn locked_liquidity(lock_program: &Pubkey, fee_nft_mint: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[LOCKED_LIQUIDITY_SEED, fee_nft_mint.as_ref()], lock_program).0
    }

    pub struct LockKeys {
        pub program: Pubkey,
        pub authority: Pubkey,
        pub payer: Pubkey,
        pub liquidity_owner: Pubkey,
        pub fee_nft_owner: Pubkey,
        pub fee_nft_mint: Pubkey,
        pub fee_nft_account: Pubkey,
        pub pool_state: Pubkey,
        pub locked_liquidity: Pubkey,
        pub lp_mint: Pubkey,
        pub liquidity_owner_lp: Pubkey,
        pub locked_lp_vault: Pubkey,
        pub token_0_vault: Pubkey,
        pub token_1_vault: Pubkey,
        pub metadata_account: Pubkey,
        pub metadata_program: Pubkey,
    }

    pub fn lock_cp_liquidity_ix(k: &LockKeys, lp_amount: u64) -> Instruction {
        let mut data = IX_LOCK_CP_LIQUIDITY.to_vec();
        data.extend_from_slice(&lp_amount.to_le_bytes());
        data.push(0); // with_metadata = false
        Instruction {
            program_id: k.program,
            accounts: vec![
                AccountMeta::new_readonly(k.authority, false),
                AccountMeta::new(k.payer, true),
                AccountMeta::new_readonly(k.liquidity_owner, true),
                AccountMeta::new_readonly(k.fee_nft_owner, false),
                AccountMeta::new(k.fee_nft_mint, true),
                AccountMeta::new(k.fee_nft_account, false),
                AccountMeta::new_readonly(k.pool_state, false),
                AccountMeta::new(k.locked_liquidity, false),
                AccountMeta::new(k.lp_mint, false),
                AccountMeta::new(k.liquidity_owner_lp, false),
                AccountMeta::new(k.locked_lp_vault, false),
                AccountMeta::new(k.token_0_vault, false),
                AccountMeta::new(k.token_1_vault, false),
                AccountMeta::new(k.metadata_account, false),
                AccountMeta::new_readonly(anchor_lang::solana_program::sysvar::rent::ID, false),
                AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
                AccountMeta::new_readonly(anchor_spl::token::ID, false),
                AccountMeta::new_readonly(anchor_spl::associated_token::ID, false),
                AccountMeta::new_readonly(k.metadata_program, false),
            ],
            data,
        }
    }

    pub struct CollectKeys {
        pub program: Pubkey,
        pub authority: Pubkey,
        pub fee_nft_owner: Pubkey,
        pub fee_nft_account: Pubkey,
        pub locked_liquidity: Pubkey,
        pub cpmm_program: Pubkey,
        pub cp_authority: Pubkey,
        pub pool_state: Pubkey,
        pub lp_mint: Pubkey,
        pub recipient_token_0: Pubkey,
        pub recipient_token_1: Pubkey,
        pub token_0_vault: Pubkey,
        pub token_1_vault: Pubkey,
        pub vault_0_mint: Pubkey,
        pub vault_1_mint: Pubkey,
        pub locked_lp_vault: Pubkey,
    }

    pub fn collect_cp_fees_ix(k: &CollectKeys) -> Instruction {
        let mut data = IX_COLLECT_CP_FEES.to_vec();
        data.extend_from_slice(&u64::MAX.to_le_bytes());
        Instruction {
            program_id: k.program,
            accounts: vec![
                AccountMeta::new_readonly(k.authority, false),
                AccountMeta::new_readonly(k.fee_nft_owner, true),
                AccountMeta::new_readonly(k.fee_nft_account, false),
                AccountMeta::new(k.locked_liquidity, false),
                AccountMeta::new_readonly(k.cpmm_program, false),
                AccountMeta::new_readonly(k.cp_authority, false),
                AccountMeta::new(k.pool_state, false),
                AccountMeta::new(k.lp_mint, false),
                AccountMeta::new(k.recipient_token_0, false),
                AccountMeta::new(k.recipient_token_1, false),
                AccountMeta::new(k.token_0_vault, false),
                AccountMeta::new(k.token_1_vault, false),
                AccountMeta::new_readonly(k.vault_0_mint, false),
                AccountMeta::new_readonly(k.vault_1_mint, false),
                AccountMeta::new(k.locked_lp_vault, false),
                AccountMeta::new_readonly(anchor_spl::token::ID, false),
                AccountMeta::new_readonly(TOKEN_2022, false),
                AccountMeta::new_readonly(MEMO_PROGRAM, false),
            ],
            data,
        }
    }
}
