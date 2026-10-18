//! Hand-built CPIs into Velvet Pools (the launchpad's own pools for bonded coins).
use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};

pub mod program {
    use anchor_lang::prelude::declare_id;
    declare_id!("VPkPeiAg7LheTtDdBByCmXewabF9zF6yK5YYLmfPjWb");
}
pub use program::ID;

const INITIALIZE: [u8; 8] = [175, 175, 109, 31, 13, 152, 155, 237];
const COLLECT_FEES: [u8; 8] = [164, 152, 207, 99, 30, 186, 19, 182];

pub fn pool(mint: &Pubkey) -> Pubkey { Pubkey::find_program_address(&[b"pool", mint.as_ref()], &ID).0 }
pub fn vault(pool: &Pubkey, mint: &Pubkey) -> Pubkey { Pubkey::find_program_address(&[b"vault", pool.as_ref(), mint.as_ref()], &ID).0 }
pub fn vault_auth() -> Pubkey { Pubkey::find_program_address(&[b"vault_auth"], &ID).0 }

#[allow(clippy::too_many_arguments)]
pub fn initialize_ix(creator: Pubkey, token_mint: Pubkey, wsol: Pubkey, creator_sol: Pubkey, creator_token: Pubkey, launch: Pubkey, grad_bump: u8, sol_amount: u64, token_amount: u64) -> Instruction {
    let pool = pool(&token_mint);
    let mut data = INITIALIZE.to_vec();
    data.extend_from_slice(launch.as_ref());
    data.push(grad_bump);
    data.extend_from_slice(&sol_amount.to_le_bytes());
    data.extend_from_slice(&token_amount.to_le_bytes());
    Instruction {
        program_id: ID,
        accounts: vec![
            AccountMeta::new(creator, true),
            AccountMeta::new_readonly(token_mint, false),
            AccountMeta::new_readonly(wsol, false),
            AccountMeta::new(pool, false),
            AccountMeta::new_readonly(vault_auth(), false),
            AccountMeta::new(vault(&pool, &wsol), false),
            AccountMeta::new(vault(&pool, &token_mint), false),
            AccountMeta::new(creator_sol, false),
            AccountMeta::new(creator_token, false),
            AccountMeta::new_readonly(anchor_spl::token::ID, false),
            AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
        ],
        data,
    }
}

pub fn collect_fees_ix(owner: Pubkey, token_mint: Pubkey, destination: Pubkey) -> Instruction {
    let pool = pool(&token_mint);
    let wsol = anchor_spl::token::spl_token::native_mint::ID;
    Instruction {
        program_id: ID,
        accounts: vec![
            AccountMeta::new_readonly(owner, true),
            AccountMeta::new(pool, false),
            AccountMeta::new_readonly(vault_auth(), false),
            AccountMeta::new(vault(&pool, &wsol), false),
            AccountMeta::new(destination, false),
            AccountMeta::new_readonly(anchor_spl::token::ID, false),
        ],
        data: COLLECT_FEES.to_vec(),
    }
}
