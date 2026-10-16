//! FairLaunch v2 end-to-end tests. The compiled program runs next to the real
//! Raydium CPMM and Metaplex Token Metadata bytecode dumped from devnet.

use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use base64::Engine;
use fairlaunch::state::*;
use fairlaunch::raydium;
use litesvm::LiteSVM;
use solana_sdk::{
    account::Account,
    clock::Clock,
    compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction},
    program_pack::Pack,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_program, sysvar,
    transaction::Transaction,
};
use spl_associated_token_account::get_associated_token_address as ata;
use std::str::FromStr;

const SOL: u64 = 1_000_000_000;
const AUCTION: u64 = 40;
const DECAY: u64 = 150;
const CAP: u64 = 5 * SOL;
const CT70: u64 = 700_000_000_000_000;
const VT70: u64 = 1_225_000_000_000_000;
const V70: u64 = 75 * SOL;
/// Most a wallet can commit to a fresh 70/30 line under the 3% cap (≈1.90 SOL).
const LINE_MAX: u64 = 1_900_000_000;

fn pid() -> Pubkey { fairlaunch::ID }
fn cpmm() -> Pubkey { Pubkey::from_str("DRaycpLY18LhpbydsBWbVJtxpNv9oXPgjRSfpF2bWpYb").unwrap() }
fn mpl() -> Pubkey { Pubkey::from_str("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s").unwrap() }
fn amm_config() -> Pubkey { Pubkey::from_str("5MxLgy9oPdTC3YgkiePHqr3EoCRD9uLVYRQS2ANAs7wy").unwrap() }
fn lockp() -> Pubkey { Pubkey::from_str("DRay25Usp3YJAi7beckgpGUC7mGJ2cR1AVPxhYfwVCUX").unwrap() }
fn pool_fee_rx() -> Pubkey { Pubkey::from_str("3oE58BKVt8KuYkGxx8zBojugnymWmBiyafWgMrnb6eYy").unwrap() }
fn wsol() -> Pubkey { spl_token::native_mint::ID }

struct Env {
    svm: LiteSVM,
    admin: Keypair,
    distributor: Keypair,
    treasury: Pubkey,
    config: Pubkey,
}

fn load_fixture(svm: &mut LiteSVM, file: &str) {
    let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    let a = &j["account"];
    let data = base64::engine::general_purpose::STANDARD.decode(a["data"][0].as_str().unwrap()).unwrap();
    let key = Pubkey::from_str(j["pubkey"].as_str().unwrap()).unwrap();
    svm.set_account(key, Account {
        lamports: a["lamports"].as_u64().unwrap(),
        data,
        owner: Pubkey::from_str(a["owner"].as_str().unwrap()).unwrap(),
        executable: false,
        rent_epoch: 0,
    }).unwrap();
}

fn params(env_treasury: Pubkey, distributor: Pubkey) -> ConfigParams {
    ConfigParams {
        treasury: env_treasury,
        distributor,
        cpmm_program: cpmm(),
        amm_config: amm_config(),
        lock_program: lockp(),
        base_fee_bps: 100,
        holder_fee_bps: 25,
        max_snipe_fee_bps: 5_000,
        cover_to_holders_bps: 10_000,
        pool_fee_creator_bps: 2_500,
        pool_fee_holders_bps: 2_500,
        decay_slots: DECAY,
        min_auction_slots: 10,
        max_auction_slots: 750,
        per_wallet_commit_cap: CAP,
        max_creator_buy: 2 * SOL,
        max_min_raise: 85 * SOL,
        max_vest_secs: 30 * 86_400,
        graduation_fee: SOL,
        // Mainnet's 70/30 curve: 700M on the curve, bonds at 100 SOL.
        virtual_sol: V70,
        virtual_tokens: VT70,
        curve_tokens: CT70,
        min_reward_epoch_secs: 3_000,
    }
}

const TIMELOCK: i64 = 172_800;
fn programdata() -> Pubkey { Pubkey::find_program_address(&[pid().as_ref()], &solana_sdk::bpf_loader_upgradeable::id()).0 }

/// Deploy the program the way a real cluster does (upgradeable loader), with
/// `authority` as its upgrade authority.
fn deploy_upgradeable(svm: &mut LiteSVM, authority: &Pubkey) {
    use solana_sdk::bpf_loader_upgradeable::UpgradeableLoaderState;
    let elf = std::fs::read("../target/deploy/fairlaunch.so").unwrap();
    let mut pd = bincode::serialize(&UpgradeableLoaderState::ProgramData { slot: 0, upgrade_authority_address: Some(*authority) }).unwrap();
    pd.resize(UpgradeableLoaderState::size_of_programdata_metadata(), 0);
    pd.extend_from_slice(&elf);
    let lamports = svm.minimum_balance_for_rent_exemption(pd.len());
    svm.set_account(programdata(), Account { lamports, data: pd, owner: solana_sdk::bpf_loader_upgradeable::id(), executable: false, rent_epoch: 0 }).unwrap();
    let prog = bincode::serialize(&UpgradeableLoaderState::Program { programdata_address: programdata() }).unwrap();
    let lamports = svm.minimum_balance_for_rent_exemption(prog.len());
    svm.set_account(pid(), Account { lamports, data: prog, owner: solana_sdk::bpf_loader_upgradeable::id(), executable: true, rent_epoch: 0 }).unwrap();
}

fn init_config_ix(env_config: Pubkey, authority: &Pubkey, p: ConfigParams) -> Instruction {
    Instruction {
        program_id: pid(),
        accounts: fairlaunch::accounts::InitConfig { authority: *authority, config: env_config, program: pid(), program_data: programdata(), system_program: system_program::ID }.to_account_metas(None),
        data: fairlaunch::instruction::InitConfig { params: p, timelock_secs: TIMELOCK }.data(),
    }
}

fn setup() -> Env { setup_with(|_| {}) }
fn setup_with(tweak: impl Fn(&mut ConfigParams)) -> Env {
    let mut svm = LiteSVM::new();
    svm.add_program_from_file(cpmm(), "fixtures/raydium_cpmm.so").unwrap();
    svm.add_program_from_file(mpl(), "fixtures/mpl_token_metadata.so").unwrap();
    svm.add_program_from_file(lockp(), "fixtures/raydium_lock.so").unwrap();
    load_fixture(&mut svm, "fixtures/amm_config_0.json");
    load_fixture(&mut svm, "fixtures/create_pool_fee.json");
    if svm.get_account(&wsol()).map(|a| a.owner != spl_token::ID).unwrap_or(true) {
        // Native mint, as it exists on every real cluster.
        let mut data = vec![0u8; spl_token::state::Mint::LEN];
        spl_token::state::Mint { mint_authority: None.into(), supply: 0, decimals: 9, is_initialized: true, freeze_authority: None.into() }.pack_into_slice(&mut data);
        svm.set_account(wsol(), Account { lamports: 1_461_600, data, owner: spl_token::ID, executable: false, rent_epoch: 0 }).unwrap();
    }
    let mut c = svm.get_sysvar::<Clock>();
    c.unix_timestamp = 1_760_000_000;
    svm.set_sysvar(&c);

    let admin = Keypair::new();
    deploy_upgradeable(&mut svm, &admin.pubkey());
    let distributor = Keypair::new();
    svm.airdrop(&admin.pubkey(), 100 * SOL).unwrap();
    svm.airdrop(&distributor.pubkey(), SOL).unwrap();
    let treasury = Keypair::new().pubkey();
    svm.airdrop(&treasury, SOL).unwrap();
    let (config, _) = Pubkey::find_program_address(&[CONFIG_SEED], &pid());
    let mut env = Env { svm, admin, distributor, treasury, config };
    let mut p = params(treasury, env.distributor.pubkey());
    tweak(&mut p);
    let ix = init_config_ix(config, &env.admin.pubkey(), p);
    let admin = env.admin.insecure_clone();
    send(&mut env.svm, vec![ix], &admin).unwrap();
    env
}

fn send(svm: &mut LiteSVM, ixs: Vec<Instruction>, payer: &Keypair) -> Result<Vec<String>, String> {
    send_multi(svm, ixs, payer, &[])
}
fn send_multi(svm: &mut LiteSVM, mut ixs: Vec<Instruction>, payer: &Keypair, extra: &[&Keypair]) -> Result<Vec<String>, String> {
    ixs.insert(0, ComputeBudgetInstruction::set_compute_unit_limit(1_400_000));
    svm.expire_blockhash();
    let mut signers = vec![payer];
    signers.extend_from_slice(extra);
    let tx = Transaction::new_signed_with_payer(&ixs, Some(&payer.pubkey()), &signers, svm.latest_blockhash());
    let size = bincode::serialize(&tx).unwrap().len();
    assert!(size <= 1232, "transaction is {size} bytes, over Solana's 1232-byte limit");
    svm.send_transaction(tx).map(|m| m.logs).map_err(|e| format!("{:?}\n{}", e.err, e.meta.logs.join("\n")))
}

fn user(svm: &mut LiteSVM, sol: u64) -> Keypair {
    let k = Keypair::new();
    svm.airdrop(&k.pubkey(), sol * SOL).unwrap();
    k
}
fn launch_state(svm: &LiteSVM, launch: &Pubkey) -> Launch {
    Launch::try_deserialize(&mut svm.get_account(launch).unwrap().data.as_slice()).unwrap()
}
fn tok(svm: &LiteSVM, a: &Pubkey) -> u64 {
    svm.get_account(a).map(|x| spl_token::state::Account::unpack(&x.data).unwrap().amount).unwrap_or(0)
}
fn bal(svm: &LiteSVM, a: &Pubkey) -> u64 { svm.get_balance(a).unwrap_or(0) }
fn slot(svm: &LiteSVM) -> u64 { svm.get_sysvar::<Clock>().slot }
fn advance(svm: &mut LiteSVM, slots: u64, secs: i64) {
    let mut c = svm.get_sysvar::<Clock>();
    c.slot += slots;
    c.unix_timestamp += secs;
    svm.set_sysvar(&c);
    svm.warp_to_slot(c.slot);
    let mut c2 = svm.get_sysvar::<Clock>();
    c2.unix_timestamp = c.unix_timestamp;
    svm.set_sysvar(&c2);
}

#[derive(Clone, Copy)]
struct L { mint: Pubkey, launch: Pubkey, vault: Pubkey, escrow: Pubkey }

fn create(env: &mut Env, creator: &Keypair, seed: [u8; 16], buy: u64, min_raise: u64, vest: i64) -> Result<L, String> {
    let (mint, _) = Pubkey::find_program_address(&[MINT_SEED, &seed], &pid());
    let (launch, _) = Pubkey::find_program_address(&[LAUNCH_SEED, mint.as_ref()], &pid());
    let (escrow, _) = Pubkey::find_program_address(&[ESCROW_SEED, launch.as_ref()], &pid());
    let (metadata, _) = Pubkey::find_program_address(&[b"metadata", mpl().as_ref(), mint.as_ref()], &mpl());
    let vault = ata(&launch, &mint);
    let ix = Instruction {
        program_id: pid(),
        accounts: fairlaunch::accounts::CreateLaunch {
            creator: creator.pubkey(), config: env.config, treasury: env.treasury, mint, launch, vault, escrow, metadata,
            metadata_program: mpl(), token_program: spl_token::ID, associated_token_program: spl_associated_token_account::ID,
            system_program: system_program::ID, rent: sysvar::rent::ID,
        }.to_account_metas(None),
        data: fairlaunch::instruction::CreateLaunch {
            seed, name: "Velvet Test".into(), symbol: "VTEST".into(), uri: "https://velvet.example/m/x.json".into(),
            auction_slots: AUCTION, creator_buy_lamports: buy, min_raise, vest_secs: vest,
        }.data(),
    };
    send(&mut env.svm, vec![ix], creator)?;
    Ok(L { mint, launch, vault, escrow })
}

fn commit(env: &mut Env, l: &L, u: &Keypair, lamports: u64) -> Result<(), String> {
    let (commitment, _) = Pubkey::find_program_address(&[COMMIT_SEED, l.launch.as_ref(), u.pubkey().as_ref()], &pid());
    let ix = Instruction {
        program_id: pid(),
        accounts: fairlaunch::accounts::Commit { user: u.pubkey(), config: env.config, launch: l.launch, commitment, system_program: system_program::ID }.to_account_metas(None),
        data: fairlaunch::instruction::Commit { lamports }.data(),
    };
    send(&mut env.svm, vec![ix], u).map(|_| ())
}
fn settle(env: &mut Env, l: &L, payer: &Keypair) -> Result<(), String> {
    let ix = Instruction {
        program_id: pid(),
        accounts: fairlaunch::accounts::Settle { config: env.config, treasury: env.treasury, launch: l.launch }.to_account_metas(None),
        data: fairlaunch::instruction::Settle {}.data(),
    };
    send(&mut env.svm, vec![ix], payer).map(|_| ())
}
fn claim(env: &mut Env, l: &L, u: &Keypair) -> Result<(), String> {
    let (commitment, _) = Pubkey::find_program_address(&[COMMIT_SEED, l.launch.as_ref(), u.pubkey().as_ref()], &pid());
    let ix = Instruction {
        program_id: pid(),
        accounts: fairlaunch::accounts::Claim {
            user: u.pubkey(), launch: l.launch, mint: l.mint, commitment, vault: l.vault, user_ata: ata(&u.pubkey(), &l.mint),
            token_program: spl_token::ID, associated_token_program: spl_associated_token_account::ID, system_program: system_program::ID,
        }.to_account_metas(None),
        data: fairlaunch::instruction::Claim {}.data(),
    };
    send(&mut env.svm, vec![ix], u).map(|_| ())
}
fn escrow_ix(l: &L, creator: &Keypair, data: Vec<u8>) -> Instruction {
    Instruction {
        program_id: pid(),
        accounts: fairlaunch::accounts::CreatorEscrow {
            creator: creator.pubkey(), launch: l.launch, mint: l.mint, escrow: l.escrow, vault: l.vault, creator_ata: ata(&creator.pubkey(), &l.mint),
            token_program: spl_token::ID, associated_token_program: spl_associated_token_account::ID, system_program: system_program::ID,
        }.to_account_metas(None),
        data,
    }
}
fn trade(env: &mut Env, l: &L, u: &Keypair, buy: bool, amount: u64) -> Result<(), String> {
    let data = if buy {
        fairlaunch::instruction::Buy { lamports_in: amount, min_tokens_out: 0 }.data()
    } else {
        fairlaunch::instruction::Sell { tokens_in: amount, min_lamports_out: 0 }.data()
    };
    let ix = Instruction {
        program_id: pid(),
        accounts: fairlaunch::accounts::TradeCtx {
            user: u.pubkey(), config: env.config, treasury: env.treasury, launch: l.launch, mint: l.mint, vault: l.vault,
            user_ata: ata(&u.pubkey(), &l.mint), token_program: spl_token::ID, associated_token_program: spl_associated_token_account::ID,
            system_program: system_program::ID,
            commitment: Pubkey::find_program_address(&[COMMIT_SEED, l.launch.as_ref(), u.pubkey().as_ref()], &pid()).0,
        }.to_account_metas(None),
        data,
    };
    send(&mut env.svm, vec![ix], u).map(|_| ())
}
fn distribute(env: &mut Env, l: &L, signer: &Keypair, pays: &[(Pubkey, u64)]) -> Result<(), String> {
    distribute_epoch(env, l, signer, 1, pays)
}
fn distribute_epoch(env: &mut Env, l: &L, signer: &Keypair, epoch: u32, pays: &[(Pubkey, u64)]) -> Result<(), String> {
    let s = launch_state(&env.svm, &l.launch);
    let pool_token_vault = if s.graduated { Some(raydium::vault(&cpmm(), &s.pool, &l.mint)) } else { None };
    let mut metas = fairlaunch::accounts::Distribute {
        distributor: signer.pubkey(), config: env.config, launch: l.launch, mint: l.mint, vault: l.vault, escrow: l.escrow, pool_token_vault,
    }.to_account_metas(None);
    for (w, _) in pays {
        metas.push(AccountMeta::new_readonly(ata(w, &l.mint), false));
        metas.push(AccountMeta::new(*w, false));
    }
    let ix = Instruction { program_id: pid(), accounts: metas, data: fairlaunch::instruction::DistributeRewards { epoch, amounts: pays.iter().map(|p| p.1).collect() }.data() };
    send(&mut env.svm, vec![ix], signer).map(|_| ())
}

fn seed(n: u8) -> [u8; 16] { let mut s = [7u8; 16]; s[0] = n; s }

/// Would a wallet with `total` lamports in this line (plus `own` tokens already, for the creator)
/// stay under the 3% cap? Mirrors the program's check: the whole commit bought alone at the
/// pre-line price.
fn line_fits(s: &Launch, total: u64, own: u64) -> bool {
    let fee = fairlaunch::math::fee_amount(total, 100).unwrap();
    let q = fairlaunch::math::quote_buy(s.virtual_sol, s.virtual_token, s.real_token, total - fee).unwrap();
    q.tokens_out + own <= MAX_WALLET_TOKENS_BONDING
}
/// The most one (non-creator) wallet can commit to this line.
fn line_max(env: &Env, l: &L) -> u64 {
    let s = launch_state(&env.svm, &l.launch);
    let (mut lo, mut hi) = (0u64, CAP);
    while lo < hi { let mid = (lo + hi + 1) / 2; if line_fits(&s, mid, 0) { lo = mid } else { hi = mid - 1 } }
    lo
}
/// `n` fresh wallets, each committing the most it may.
fn fill_line(env: &mut Env, l: &L, n: usize) -> Vec<Keypair> {
    let amt = line_max(env, l);
    let ws: Vec<Keypair> = (0..n).map(|_| user(&mut env.svm, 10)).collect();
    for w in &ws { commit(env, l, w, amt).unwrap(); }
    ws
}

// ───────────────────────────── tests ─────────────────────────────

#[test]
fn create_writes_metadata_and_escrows_creator_bag() {
    let mut env = setup();
    let creator = user(&mut env.svm, 10);
    let l = create(&mut env, &creator, seed(1), SOL, 0, 86_400).unwrap();
    let mint = spl_token::state::Mint::unpack(&env.svm.get_account(&l.mint).unwrap().data).unwrap();
    assert!(mint.mint_authority.is_none() && mint.freeze_authority.is_none());
    assert_eq!(mint.supply, TOTAL_SUPPLY);
    let (metadata, _) = Pubkey::find_program_address(&[b"metadata", mpl().as_ref(), l.mint.as_ref()], &mpl());
    let md = env.svm.get_account(&metadata).expect("metadata account exists");
    assert_eq!(md.owner, mpl());
    assert!(String::from_utf8_lossy(&md.data).contains("VTEST"));
    let s = launch_state(&env.svm, &l.launch);
    assert_eq!(tok(&env.svm, &l.escrow), s.creator_tokens);
    assert_eq!(tok(&env.svm, &ata(&creator.pubkey(), &l.mint)), 0, "creator holds nothing until vested");
    // creator buy fee: 1% of 1 SOL, a quarter to holders
    assert_eq!(s.reward_sol, SOL / 100 / 4);
}

#[test]
fn line_below_minimum_cancels_and_refunds_everyone() {
    let mut env = setup();
    let creator = user(&mut env.svm, 10);
    let l = create(&mut env, &creator, seed(2), SOL / 2, 3 * SOL, 0).unwrap();
    let a = user(&mut env.svm, 10);
    commit(&mut env, &l, &a, SOL).unwrap();
    let before = bal(&env.svm, &a.pubkey());
    advance(&mut env.svm, AUCTION, 20);
    settle(&mut env, &l, &a).unwrap();
    let s = launch_state(&env.svm, &l.launch);
    assert!(s.cancelled);
    let e = trade(&mut env, &l, &a, true, SOL / 10).unwrap_err();
    assert!(e.contains("LaunchCancelled"), "{e}");
    claim(&mut env, &l, &a).unwrap();
    assert!(bal(&env.svm, &a.pubkey()) > before + SOL - 10_000_000, "full refund (minus ATA rent)");
    let cb = bal(&env.svm, &creator.pubkey());
    let ix = escrow_ix(&l, &creator, fairlaunch::instruction::RefundCreator {}.data());
    send(&mut env.svm, vec![ix], &creator).unwrap();
    assert!(bal(&env.svm, &creator.pubkey()) > cb + SOL / 2 - SOL / 50, "creator buy refunded minus fee");
    assert_eq!(tok(&env.svm, &l.escrow), 0);
}

#[test]
fn fair_line_fee_split_and_flipper_funds_holders() {
    let mut env = setup();
    let creator = user(&mut env.svm, 10);
    let l = create(&mut env, &creator, seed(3), 0, SOL, 0).unwrap();
    let start = slot(&env.svm);
    let early = user(&mut env.svm, 20);
    let late = user(&mut env.svm, 20);
    commit(&mut env, &l, &early, 3 * SOL / 2).unwrap();
    advance(&mut env.svm, AUCTION - 2, 15);
    commit(&mut env, &l, &late, 3 * SOL / 2).unwrap();
    advance(&mut env.svm, 2, 1);
    let t0 = bal(&env.svm, &env.treasury);
    settle(&mut env, &l, &early).unwrap();
    claim(&mut env, &l, &early).unwrap();
    claim(&mut env, &l, &late).unwrap();
    assert_eq!(tok(&env.svm, &ata(&early.pubkey(), &l.mint)), tok(&env.svm, &ata(&late.pubkey(), &l.mint)));
    let s = launch_state(&env.svm, &l.launch);
    // settle fee 1% of 3 SOL: a quarter to holders, the rest to the treasury
    assert_eq!(s.reward_sol, 3 * SOL / 100 / 4);
    assert_eq!(bal(&env.svm, &env.treasury) - t0, 3 * SOL / 100 - 3 * SOL / 100 / 4);
    let _ = start;

    // Flipper at 50% cover: premium goes to holders
    let flip = user(&mut env.svm, 20);
    let r0 = s.reward_sol;
    trade(&mut env, &l, &flip, true, 2 * SOL).unwrap();
    let s2 = launch_state(&env.svm, &l.launch);
    let fee = 2 * SOL / 2; // 50%
    let base_part = fee / 50; // 1/50 of a 50% fee is the 1% base
    let expected = (fee - base_part) + base_part / 4;
    assert_eq!(s2.reward_sol - r0, expected, "cover premium + quarter of base to holders");
    // solvency
    let rent = env.svm.minimum_balance_for_rent_exemption(env.svm.get_account(&l.launch).unwrap().data.len());
    assert!(bal(&env.svm, &l.launch) >= rent + s2.liabilities());
}

#[test]
fn creator_bag_vests_linearly() {
    let mut env = setup();
    let creator = user(&mut env.svm, 10);
    let l = create(&mut env, &creator, seed(4), SOL, 0, 1000).unwrap();
    let total = launch_state(&env.svm, &l.launch).creator_tokens;
    let e = send(&mut env.svm, vec![escrow_ix(&l, &creator, fairlaunch::instruction::ClaimVested {}.data())], &creator).unwrap_err();
    assert!(e.contains("NotSettled"), "{e}");
    advance(&mut env.svm, AUCTION, 20);
    settle(&mut env, &l, &creator).unwrap();
    advance(&mut env.svm, 10, 250);
    send(&mut env.svm, vec![escrow_ix(&l, &creator, fairlaunch::instruction::ClaimVested {}.data())], &creator).unwrap();
    let got = tok(&env.svm, &ata(&creator.pubkey(), &l.mint));
    assert!(got >= total / 4 && got <= total / 4 + total / 100, "about a quarter after 25%: {got} of {total}");
    advance(&mut env.svm, 10, 2_000);
    send(&mut env.svm, vec![escrow_ix(&l, &creator, fairlaunch::instruction::ClaimVested {}.data())], &creator).unwrap();
    assert_eq!(tok(&env.svm, &ata(&creator.pubkey(), &l.mint)), total);
    let e = send(&mut env.svm, vec![escrow_ix(&l, &creator, fairlaunch::instruction::ClaimVested {}.data())], &creator).unwrap_err();
    assert!(e.contains("NothingToClaim"), "{e}");
}

#[test]
fn distributor_pays_holders_only() {
    let mut env = setup();
    let creator = user(&mut env.svm, 10);
    let l = create(&mut env, &creator, seed(5), 0, 0, 0).unwrap();
    let a = user(&mut env.svm, 20);
    let b = user(&mut env.svm, 20);
    commit(&mut env, &l, &a, 3 * SOL / 2).unwrap();
    commit(&mut env, &l, &b, SOL / 2).unwrap();
    advance(&mut env.svm, AUCTION, 20);
    settle(&mut env, &l, &a).unwrap();
    claim(&mut env, &l, &a).unwrap();
    claim(&mut env, &l, &b).unwrap();
    let flip = user(&mut env.svm, 20);
    trade(&mut env, &l, &flip, true, SOL).unwrap(); // fills the pool via cover charge
    let pool = launch_state(&env.svm, &l.launch).reward_sol;
    assert!(pool > SOL / 3);

    let outsider = user(&mut env.svm, 1);
    let dist = env.distributor.insecure_clone();
    // not a holder → rejected
    let e = distribute(&mut env, &l, &dist, &[(outsider.pubkey(), 1000)]).unwrap_err();
    assert!(e.contains("BadRewardAccount") || e.contains("AccountNotInitialized") || e.contains("InvalidAccountData"), "{e}");
    // wrong signer → rejected
    let e = distribute(&mut env, &l, &outsider, &[(a.pubkey(), 1000)]).unwrap_err();
    assert!(e.contains("NotDistributor"), "{e}");
    // overdraw → rejected
    let e = distribute(&mut env, &l, &dist, &[(a.pubkey(), pool + 1)]).unwrap_err();
    assert!(e.contains("RewardOverdraw"), "{e}");
    let (ba, bb) = (bal(&env.svm, &a.pubkey()), bal(&env.svm, &b.pubkey()));
    distribute(&mut env, &l, &dist, &[(a.pubkey(), pool * 3 / 4), (b.pubkey(), pool / 4)]).unwrap();
    assert_eq!(bal(&env.svm, &a.pubkey()) - ba, pool * 3 / 4);
    assert_eq!(bal(&env.svm, &b.pubkey()) - bb, pool / 4);
    let s = launch_state(&env.svm, &l.launch);
    assert!(s.reward_sol <= 1);
    assert_eq!(s.reward_paid, pool * 3 / 4 + pool / 4);
}

// ── graduation into Raydium ──

struct PoolKeys { pool: Pubkey, lp_mint: Pubkey, v0: Pubkey, v1: Pubkey, obs: Pubkey, m0: Pubkey, m1: Pubkey, grad: Pubkey }
fn pool_keys_at(l: &L, pool: Pubkey) -> PoolKeys {
    let (m0, m1, _) = raydium::sorted(l.mint, wsol());
    let (grad, _) = Pubkey::find_program_address(&[GRAD_SEED, l.launch.as_ref()], &pid());
    PoolKeys {
        pool, lp_mint: raydium::lp_mint(&cpmm(), &pool), v0: raydium::vault(&cpmm(), &pool, &m0), v1: raydium::vault(&cpmm(), &pool, &m1),
        obs: raydium::observation(&cpmm(), &pool), m0, m1, grad,
    }
}
fn pool_keys(env: &Env, l: &L) -> PoolKeys { pool_keys_at(l, launch_state(&env.svm, &l.launch).pool) }
fn graduate(env: &mut Env, l: &L, payer: &Keypair) -> Result<Vec<String>, String> {
    let pool_kp = Keypair::new();
    graduate_with(env, l, payer, &pool_kp)
}
fn graduate_with(env: &mut Env, l: &L, payer: &Keypair, pool_kp: &Keypair) -> Result<Vec<String>, String> {
    let k = pool_keys_at(l, pool_kp.pubkey());
    let prep = Instruction {
        program_id: pid(),
        accounts: fairlaunch::accounts::PrepareGraduation { payer: payer.pubkey(), config: env.config, treasury: env.treasury, launch: l.launch, grad_auth: k.grad }.to_account_metas(None),
        data: fairlaunch::instruction::PrepareGraduation {}.data(),
    };
    let ix = Instruction {
        program_id: pid(),
        accounts: fairlaunch::accounts::Graduate {
            payer: payer.pubkey(), config: env.config, treasury: env.treasury, launch: l.launch, mint: l.mint, vault: l.vault,
            grad_auth: k.grad, grad_wsol: ata(&k.grad, &wsol()), grad_token: ata(&k.grad, &l.mint), grad_lp: ata(&k.grad, &k.lp_mint),
            wsol_mint: wsol(), cpmm_program: cpmm(), amm_config: amm_config(), ray_authority: raydium::authority(&cpmm()),
            pool_state: k.pool, lp_mint: k.lp_mint, token_0_vault: k.v0, token_1_vault: k.v1, create_pool_fee: pool_fee_rx(),
            observation: k.obs, token_program: spl_token::ID, associated_token_program: spl_associated_token_account::ID,
            system_program: system_program::ID, rent: sysvar::rent::ID,
        }.to_account_metas(None),
        data: fairlaunch::instruction::Graduate {}.data(),
    };
    send_multi(&mut env.svm, vec![prep, ix], payer, &[pool_kp])
}

fn swap_sol_for_token(env: &mut Env, l: &L, u: &Keypair, lamports: u64) {
    let k = pool_keys(env, l);
    let user_wsol = ata(&u.pubkey(), &wsol());
    let user_tok = ata(&u.pubkey(), &l.mint);
    let mut ixs = vec![
        spl_associated_token_account::instruction::create_associated_token_account_idempotent(&u.pubkey(), &u.pubkey(), &wsol(), &spl_token::ID),
        spl_associated_token_account::instruction::create_associated_token_account_idempotent(&u.pubkey(), &u.pubkey(), &l.mint, &spl_token::ID),
        solana_sdk::system_instruction::transfer(&u.pubkey(), &user_wsol, lamports),
        spl_token::instruction::sync_native(&spl_token::ID, &user_wsol).unwrap(),
    ];
    let (in_vault, out_vault) = if k.m0 == wsol() { (k.v0, k.v1) } else { (k.v1, k.v0) };
    let mut data = vec![143, 190, 90, 218, 196, 30, 51, 222];
    data.extend_from_slice(&lamports.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes());
    ixs.push(Instruction {
        program_id: cpmm(),
        accounts: vec![
            AccountMeta::new(u.pubkey(), true),
            AccountMeta::new_readonly(raydium::authority(&cpmm()), false),
            AccountMeta::new_readonly(amm_config(), false),
            AccountMeta::new(k.pool, false),
            AccountMeta::new(user_wsol, false),
            AccountMeta::new(user_tok, false),
            AccountMeta::new(in_vault, false),
            AccountMeta::new(out_vault, false),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new_readonly(wsol(), false),
            AccountMeta::new_readonly(l.mint, false),
            AccountMeta::new(k.obs, false),
        ],
        data,
    });
    send(&mut env.svm, ixs, u).unwrap();
}

fn lock_liquidity(env: &mut Env, l: &L, payer: &Keypair) -> Result<Pubkey, String> {
    let k = pool_keys(env, l);
    let nft = Keypair::new();
    let lock_auth = raydium::lock::authority(&lockp());
    let (nft_md, _) = Pubkey::find_program_address(&[b"metadata", mpl().as_ref(), nft.pubkey().as_ref()], &mpl());
    let ix = Instruction {
        program_id: pid(),
        accounts: fairlaunch::accounts::LockLiquidity {
            payer: payer.pubkey(), config: env.config, launch: l.launch, grad_auth: k.grad, grad_lp: ata(&k.grad, &k.lp_mint),
            fee_nft_mint: nft.pubkey(), fee_nft_account: ata(&k.grad, &nft.pubkey()), nft_metadata: nft_md, lock_program: lockp(),
            lock_authority: lock_auth, locked_liquidity: raydium::lock::locked_liquidity(&lockp(), &nft.pubkey()),
            locked_lp_vault: ata(&lock_auth, &k.lp_mint), pool_state: k.pool, lp_mint: k.lp_mint, token_0_vault: k.v0, token_1_vault: k.v1,
            metadata_program: mpl(), token_program: spl_token::ID, associated_token_program: spl_associated_token_account::ID,
            system_program: system_program::ID, rent: sysvar::rent::ID,
        }.to_account_metas(None),
        data: fairlaunch::instruction::LockLiquidity {}.data(),
    };
    send_multi(&mut env.svm, vec![ix], payer, &[&nft])?;
    Ok(nft.pubkey())
}

fn collect_fees(env: &mut Env, l: &L, creator: &Pubkey, payer: &Keypair) -> Result<Vec<String>, String> {
    let k = pool_keys(env, l);
    let s = launch_state(&env.svm, &l.launch);
    let lock_auth = raydium::lock::authority(&lockp());
    let ix = Instruction {
        program_id: pid(),
        accounts: fairlaunch::accounts::CollectPoolFees {
            payer: payer.pubkey(), config: env.config, treasury: env.treasury, launch: l.launch, creator: *creator, mint: l.mint,
            grad_auth: k.grad, grad_wsol: ata(&k.grad, &wsol()), grad_token: ata(&k.grad, &l.mint), wsol_mint: wsol(),
            cpmm_program: cpmm(), ray_authority: raydium::authority(&cpmm()), pool_state: k.pool, lp_mint: k.lp_mint,
            token_0_vault: k.v0, token_1_vault: k.v1, lock_program: lockp(), lock_authority: lock_auth,
            locked_liquidity: raydium::lock::locked_liquidity(&lockp(), &s.fee_nft_mint), fee_nft_account: ata(&k.grad, &s.fee_nft_mint),
            locked_lp_vault: ata(&lock_auth, &k.lp_mint), token_2022_program: raydium::lock::TOKEN_2022, memo_program: raydium::lock::MEMO_PROGRAM,
            token_program: spl_token::ID, associated_token_program: spl_associated_token_account::ID, system_program: system_program::ID,
            grad_lp: ata(&k.grad, &k.lp_mint),
        }.to_account_metas(None),
        data: fairlaunch::instruction::CollectPoolFees {}.data(),
    };
    send(&mut env.svm, vec![ix], payer)
}

#[test]
fn graduates_into_raydium_burns_lp_and_collects_creator_fees() {
    let mut env = setup();
    let creator = user(&mut env.svm, 10);
    let l = create(&mut env, &creator, seed(6), SOL, 0, 0).unwrap();
    let e = graduate(&mut env, &l, &creator).unwrap_err();
    assert!(e.contains("NotComplete"), "{e}");
    let whales = fill_line(&mut env, &l, 60); // ~114 SOL at ≤3% each fills the 100 SOL curve
    advance(&mut env.svm, AUCTION, 20);
    settle(&mut env, &l, &whales[0]).unwrap();
    let s = launch_state(&env.svm, &l.launch);
    assert!(s.complete && s.real_token == 0);
    for w in &whales[..5] { claim(&mut env, &l, w).unwrap(); } // some claim before graduation

    let t0 = bal(&env.svm, &env.treasury);
    let logs = graduate(&mut env, &l, &whales[1]).unwrap();
    println!("graduate CU: {:?}", logs.iter().find(|x| x.contains(&format!("{} consumed", pid()))));
    let s = launch_state(&env.svm, &l.launch);
    let k = pool_keys(&env, &l);
    assert!(s.graduated);
    assert_ne!(s.pool, raydium::pool_state(&cpmm(), &amm_config(), &k.m0, &k.m1), "pool is not at the predictable PDA");
    assert_eq!(env.svm.get_account(&k.pool).unwrap().owner, cpmm(), "Raydium pool exists");
    let lp_held = tok(&env.svm, &ata(&k.grad, &k.lp_mint));
    assert!(lp_held > 0, "LP waits with the graduation authority");
    // second transaction: lock it forever with Raydium's locker
    let nft = lock_liquidity(&mut env, &l, &whales[3]).unwrap();
    let lock_auth = raydium::lock::authority(&lockp());
    assert_eq!(tok(&env.svm, &ata(&k.grad, &k.lp_mint)), 0, "no LP left outside the locker");
    assert_eq!(tok(&env.svm, &ata(&lock_auth, &k.lp_mint)), lp_held, "all LP locked");
    assert_eq!(tok(&env.svm, &ata(&k.grad, &nft)), 1, "fee key NFT held by the program's PDA");
    assert!(lock_liquidity(&mut env, &l, &whales[3]).is_err(), "cannot lock twice");
    let (tv, sv) = if k.m0 == wsol() { (k.v1, k.v0) } else { (k.v0, k.v1) };
    assert_eq!(tok(&env.svm, &tv), s.lp_reserve, "pool holds the LP reserve");
    assert!(tok(&env.svm, &sv) > 80 * SOL, "pool holds the curve SOL: {}", tok(&env.svm, &sv));
    assert!(bal(&env.svm, &env.treasury) - t0 >= SOL, "graduation fee (plus unused overhead) to treasury");
    // late claimers still get their tokens from the vault after graduation
    for w in &whales[5..] { claim(&mut env, &l, w).unwrap(); }
    let held: u64 = whales.iter().map(|w| tok(&env.svm, &ata(&w.pubkey(), &l.mint))).sum();
    assert!(held <= s.batch_tokens && s.batch_tokens - held < 100);
    // curve trading is closed
    let e = trade(&mut env, &l, &whales[0], true, SOL).unwrap_err();
    assert!(e.contains("CurveComplete"), "{e}");

    // trade on Raydium, then collect the creator fee
    advance(&mut env.svm, 5, 10);
    let trader = user(&mut env.svm, 50);
    swap_sol_for_token(&mut env, &l, &trader, 20 * SOL);
    assert!(tok(&env.svm, &ata(&trader.pubkey(), &l.mint)) > 0);
    let (c0, r0, t1) = (bal(&env.svm, &creator.pubkey()), launch_state(&env.svm, &l.launch).reward_sol, bal(&env.svm, &env.treasury));
    let logs = collect_fees(&mut env, &l, &creator.pubkey(), &whales[2]).unwrap();
    println!("collect CU: {:?}", logs.iter().find(|x| x.contains(&format!("{} consumed", pid()))));
    let s = launch_state(&env.svm, &l.launch);
    let (dc, dr, dt) = (bal(&env.svm, &creator.pubkey()) - c0, s.reward_sol - r0, bal(&env.svm, &env.treasury) - t1);
    // LP fee on idx0: 0.25% trade fee minus 16% protocol/fund = 0.21% of the 20 SOL input,
    // paid out half in SOL and half in the coin (by pool value), so ~0.021 SOL on the SOL side.
    // A quarter of Velvet's share went back into the pool as burned-LP liquidity.
    let rec: LiquidityRecycled = event(&logs).expect("LiquidityRecycled event");
    assert_eq!(s.pool_fees_collected, dc + dr + dt + rec.sol);
    let treasury_share = s.pool_fees_collected - s.pool_fees_collected * 2_500 / 10_000 * 2;
    assert!(rec.sol > 0 && rec.sol <= treasury_share / 4 + 1 && rec.sol + 5 >= treasury_share / 4, "recycled {} of {treasury_share}", rec.sol);
    assert!(rec.tokens > 0 && rec.lp_burned > 0);
    assert_eq!(tok(&env.svm, &ata(&k.grad, &k.lp_mint)), 0, "recycled LP was burned");
    // (the locker realises fees by withdrawing a sliver of the locked LP; nothing else touches it)
    assert!(tok(&env.svm, &ata(&lock_auth, &k.lp_mint)) <= lp_held);
    assert!(s.pool_fees_collected > 15_000_000 && s.pool_fees_collected < 30_000_000, "{}", s.pool_fees_collected);
    assert!(dc > 0 && dr > 0 && dt > dc, "creator {dc}, holders {dr}, treasury {dt}");
    // and again later (WSOL account was closed; Raydium recreates it)
    swap_sol_for_token(&mut env, &l, &trader, 4 * SOL);
    collect_fees(&mut env, &l, &creator.pubkey(), &whales[2]).unwrap();
    println!("pool fees collected: {} lamports; creator {dc}, holders {dr}, treasury {dt}", launch_state(&env.svm, &l.launch).pool_fees_collected);
    // hold-streak payouts keep working after graduation (pool vault excluded from supply)
    let dist = env.distributor.insecure_clone();
    let before = bal(&env.svm, &whales[6].pubkey());
    distribute_epoch(&mut env, &l, &dist, 1, &[(whales[6].pubkey(), 10_000)]).unwrap();
    assert_eq!(bal(&env.svm, &whales[6].pubkey()) - before, 10_000);
}

/// Decode the first Anchor event of type T from a transaction's logs.
fn event<T: anchor_lang::AnchorDeserialize + anchor_lang::Discriminator>(logs: &[String]) -> Option<T> {
    logs.iter().filter_map(|l| l.strip_prefix("Program data: ")).find_map(|b64| {
        let d = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
        if d.len() < 8 || &d[..8] != T::DISCRIMINATOR { return None; }
        T::deserialize(&mut &d[8..]).ok()
    })
}

// ───────────────────────────── free launches ─────────────────────────────

const FREE_NAME: &str = "Free Coin";
const FREE_SYM: &str = "FREE";
const FREE_URI: &str = "https://velvet.example/m/free.json";

fn approval_text(digest: &[u8; 32], name: &str, symbol: &str) -> Vec<u8> {
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("Velvet launch approval\nLaunch {name} (${symbol}) on Velvet. Signing is free.\nApproval: {hex}").into_bytes()
}

struct Free { seed: [u8; 16], name: &'static str, auction: u64, min_raise: u64, vest: i64, expires: i64 }

fn free(seed_n: u8, expires: i64) -> Free { Free { seed: seed(seed_n), name: FREE_NAME, auction: AUCTION, min_raise: 0, vest: 0, expires } }

fn free_keys(seed: &[u8; 16]) -> L {
    let (mint, _) = Pubkey::find_program_address(&[MINT_SEED, seed], &pid());
    let (launch, _) = Pubkey::find_program_address(&[LAUNCH_SEED, mint.as_ref()], &pid());
    let (escrow, _) = Pubkey::find_program_address(&[ESCROW_SEED, launch.as_ref()], &pid());
    L { mint, launch, vault: ata(&launch, &mint), escrow }
}

/// [Ed25519 approval by `signer` over `msg`, create_launch_signed for `creator` paid by `payer`].
fn free_ixs(env: &Env, f: &Free, signer: &Keypair, creator: Pubkey, payer: &Pubkey, msg: Option<Vec<u8>>) -> Vec<Instruction> {
    let digest = fairlaunch::approval_digest(&f.seed, f.name, FREE_SYM, FREE_URI, f.auction, f.min_raise, f.vest, f.expires);
    let msg = msg.unwrap_or_else(|| approval_text(&digest, f.name, FREE_SYM));
    let sig: [u8; 64] = signer.sign_message(&msg).as_ref().try_into().unwrap();
    let ed = solana_sdk::ed25519_instruction::new_ed25519_instruction_with_signature(&msg, &sig, &signer.pubkey().to_bytes());
    let l = free_keys(&f.seed);
    let (metadata, _) = Pubkey::find_program_address(&[b"metadata", mpl().as_ref(), l.mint.as_ref()], &mpl());
    let ix = Instruction {
        program_id: pid(),
        accounts: fairlaunch::accounts::CreateLaunchSigned {
            payer: *payer, config: env.config, mint: l.mint, launch: l.launch, vault: l.vault, escrow: l.escrow, metadata,
            instructions: sysvar::instructions::ID, metadata_program: mpl(), token_program: spl_token::ID,
            associated_token_program: spl_associated_token_account::ID, system_program: system_program::ID, rent: sysvar::rent::ID,
        }.to_account_metas(None),
        data: fairlaunch::instruction::CreateLaunchSigned {
            seed: f.seed, name: f.name.into(), symbol: FREE_SYM.into(), uri: FREE_URI.into(), auction_slots: f.auction,
            min_raise: f.min_raise, vest_secs: f.vest, expires_at: f.expires, creator,
        }.data(),
    };
    vec![ed, ix]
}
fn commit_ix(env: &Env, l: &L, u: &Pubkey, lamports: u64) -> Instruction {
    let (commitment, _) = Pubkey::find_program_address(&[COMMIT_SEED, l.launch.as_ref(), u.as_ref()], &pid());
    Instruction {
        program_id: pid(),
        accounts: fairlaunch::accounts::Commit { user: *u, config: env.config, launch: l.launch, commitment, system_program: system_program::ID }.to_account_metas(None),
        data: fairlaunch::instruction::Commit { lamports }.data(),
    }
}
fn now(svm: &LiteSVM) -> i64 { svm.get_sysvar::<Clock>().unix_timestamp }

#[test]
fn free_launch_creator_pays_nothing_and_first_buyer_deploys() {
    let mut env = setup();
    let creator = Keypair::new(); // no SOL at all
    let buyer = user(&mut env.svm, 10);
    let f = free(40, now(&env.svm) + 86_400);
    let mut ixs = free_ixs(&env, &f, &creator, creator.pubkey(), &buyer.pubkey(), None);
    let l = free_keys(&f.seed);
    ixs.push(commit_ix(&env, &l, &buyer.pubkey(), SOL));
    let b0 = bal(&env.svm, &buyer.pubkey());
    let logs = send(&mut env.svm, ixs, &buyer).unwrap();
    println!("free launch + first commit CU: {:?}", logs.iter().find(|x| x.contains(&format!("{} consumed", pid()))));
    assert_eq!(bal(&env.svm, &creator.pubkey()), 0, "creator spent nothing");
    let spent = b0 - bal(&env.svm, &buyer.pubkey());
    assert!(spent > SOL && spent < SOL + 30_000_000, "buyer paid the commit plus ~0.02 SOL of rent: {spent}");
    let s = launch_state(&env.svm, &l.launch);
    assert_eq!(s.creator, creator.pubkey());
    assert_eq!((s.creator_buy_sol, s.creator_tokens, s.committers, s.total_committed), (0, 0, 1, SOL));
    assert_eq!(tok(&env.svm, &l.vault), TOTAL_SUPPLY);
    let sp: LaunchSponsored = event(&logs).unwrap();
    assert_eq!((sp.creator, sp.payer), (creator.pubkey(), buyer.pubkey()));
    let md = Pubkey::find_program_address(&[b"metadata", mpl().as_ref(), l.mint.as_ref()], &mpl()).0;
    assert_eq!(env.svm.get_account(&md).unwrap().owner, mpl(), "metadata written");
    // it's a normal coin from here on
    advance(&mut env.svm, AUCTION, 20);
    settle(&mut env, &l, &buyer).unwrap();
    claim(&mut env, &l, &buyer).unwrap();
    assert!(tok(&env.svm, &ata(&buyer.pubkey(), &l.mint)) > 0);
    // the same approval can't deploy a second coin
    let again = free_ixs(&env, &f, &creator, creator.pubkey(), &buyer.pubkey(), None);
    assert!(send(&mut env.svm, again, &buyer).is_err());
}

#[test]
fn forged_tampered_or_stale_approvals_are_rejected() {
    let mut env = setup();
    let victim = Keypair::new();
    let attacker = user(&mut env.svm, 10);
    let t = now(&env.svm);
    let bad = |e: Result<Vec<String>, String>, want: &str| { let e = e.unwrap_err(); assert!(e.contains(want), "expected {want}: {e}"); };

    // someone else's signature claiming to be the victim
    let f = free(41, t + 3_600);
    bad({ let ixs = free_ixs(&env, &f, &attacker, victim.pubkey(), &attacker.pubkey(), None); send(&mut env.svm, ixs, &attacker) }, "BadApproval");
    // victim approved one coin; the attacker submits different parameters
    let signed = free(42, t + 3_600);
    let digest = fairlaunch::approval_digest(&signed.seed, signed.name, FREE_SYM, FREE_URI, signed.auction, signed.min_raise, signed.vest, signed.expires);
    let msg = approval_text(&digest, signed.name, FREE_SYM);
    let mut changed = free(42, t + 3_600);
    changed.min_raise = 3 * SOL;
    bad({ let ixs = free_ixs(&env, &changed, &victim, victim.pubkey(), &attacker.pubkey(), Some(msg.clone())); send(&mut env.svm, ixs, &attacker) }, "BadApproval");
    changed = free(42, t + 3_600);
    changed.name = "Other";
    bad({ let ixs = free_ixs(&env, &changed, &victim, victim.pubkey(), &attacker.pubkey(), Some(msg)); send(&mut env.svm, ixs, &attacker) }, "BadApproval");
    // no approval instruction at all
    let mut ixs = free_ixs(&env, &free(43, t + 3_600), &victim, victim.pubkey(), &attacker.pubkey(), None);
    ixs.remove(0);
    bad(send(&mut env.svm, ixs, &attacker), "BadApproval");
    // a signature over text that isn't a Velvet approval (e.g. a login message)
    let f = free(44, t + 3_600);
    let d = fairlaunch::approval_digest(&f.seed, f.name, FREE_SYM, FREE_URI, f.auction, f.min_raise, f.vest, f.expires);
    let hex: String = d.iter().map(|b| format!("{b:02x}")).collect();
    let login = format!("Sign in to some site\nApproval: {hex}").into_bytes();
    bad({ let ixs = free_ixs(&env, &f, &victim, victim.pubkey(), &attacker.pubkey(), Some(login)); send(&mut env.svm, ixs, &attacker) }, "BadApproval");
    // expired, or valid for too long
    bad({ let ixs = free_ixs(&env, &free(45, t - 1), &victim, victim.pubkey(), &attacker.pubkey(), None); send(&mut env.svm, ixs, &attacker) }, "ApprovalExpired");
    bad({ let ixs = free_ixs(&env, &free(46, t + 31 * 86_400), &victim, victim.pubkey(), &attacker.pubkey(), None); send(&mut env.svm, ixs, &attacker) }, "BadApproval");
    // a valid approval still works (the failures above weren't something else)
    { let ixs = free_ixs(&env, &free(47, t + 3_600), &victim, victim.pubkey(), &attacker.pubkey(), None); send(&mut env.svm, ixs, &attacker) }.unwrap();
    assert_eq!(launch_state(&env.svm, &free_keys(&seed(47)).launch).creator, victim.pubkey());
}

#[test]
fn no_wallet_can_buy_past_three_percent_while_bonding() {
    let mut env = setup();
    let creator = user(&mut env.svm, 10);
    let l = create(&mut env, &creator, seed(48), 0, 0, 0).unwrap();
    let a = user(&mut env.svm, 20);
    commit(&mut env, &l, &a, SOL / 10).unwrap();
    advance(&mut env.svm, AUCTION + DECAY, 400);
    settle(&mut env, &l, &a).unwrap();
    let whale = user(&mut env.svm, 50);
    let e = trade(&mut env, &l, &whale, true, 20 * SOL).unwrap_err();
    assert!(e.contains("WalletCapExceeded"), "{e}");
    // small buys are fine until the wallet reaches 3%
    trade(&mut env, &l, &whale, true, SOL / 2).unwrap();
    let mut n = 0;
    while trade(&mut env, &l, &whale, true, SOL / 2).is_ok() { n += 1; assert!(n < 100); }
    let held = tok(&env.svm, &ata(&whale.pubkey(), &l.mint));
    assert!(held <= MAX_WALLET_TOKENS_BONDING && held > MAX_WALLET_TOKENS_BONDING * 8 / 10, "held {held}");
    // selling is never limited
    trade(&mut env, &l, &whale, false, held / 2).unwrap();
}

/// The 3% cap also holds in the line and for the creator's opening buy: nobody can get more
/// than 3% of the supply before the coin bonds, even alone in the line.
#[test]
fn the_line_and_the_opening_buy_are_held_to_three_percent() {
    let mut env = setup();
    // opening buy: 2 SOL at the start price would be ~31.5M tokens (>3%), so it's refused
    let creator = user(&mut env.svm, 10);
    let e = match create(&mut env, &creator, seed(50), 2 * SOL, 0, 0) { Err(e) => e, Ok(_) => panic!("2 SOL opening buy was accepted") };
    assert!(e.contains("WalletCapExceeded"), "{e}");
    let l = create(&mut env, &creator, seed(50), SOL, 0, 0).unwrap();
    let own = launch_state(&env.svm, &l.launch).creator_tokens;
    assert!(own > 0 && own <= MAX_WALLET_TOKENS_BONDING);

    // a lone wallet can commit up to ~1.9 SOL; one lamport more is refused, also in two steps
    let most = line_max(&env, &l);
    assert!(most > 17 * SOL / 10 && most < 2 * SOL, "line max {most}");
    let a = user(&mut env.svm, 10);
    let e = commit(&mut env, &l, &a, most + 1).unwrap_err();
    assert!(e.contains("WalletCapExceeded"), "{e}");
    commit(&mut env, &l, &a, most - SOL / 2).unwrap();
    let e = commit(&mut env, &l, &a, SOL / 2 + 1).unwrap_err();
    assert!(e.contains("WalletCapExceeded"), "{e}");
    commit(&mut env, &l, &a, SOL / 2).unwrap();

    // the creator's bag counts against their own line spot
    let s = launch_state(&env.svm, &l.launch);
    let mut room = 0u64;
    while line_fits(&s, room + SOL / 100, own) { room += SOL / 100; }
    let e = commit(&mut env, &l, &creator, room + SOL / 100).unwrap_err();
    assert!(e.contains("WalletCapExceeded"), "{e}");
    commit(&mut env, &l, &creator, room).unwrap();

    // alone or not, nobody ends up above 3% after the doors open
    advance(&mut env.svm, AUCTION, 20);
    settle(&mut env, &l, &a).unwrap();
    claim(&mut env, &l, &a).unwrap();
    claim(&mut env, &l, &creator).unwrap();
    assert!(tok(&env.svm, &ata(&a.pubkey(), &l.mint)) <= MAX_WALLET_TOKENS_BONDING);
    assert!(tok(&env.svm, &ata(&creator.pubkey(), &l.mint)) + own <= MAX_WALLET_TOKENS_BONDING);
}

/// Buying on the curve before claiming doesn't get around the cap: an unclaimed line share and
/// the creator's bag still in escrow both count.
#[test]
fn unclaimed_line_share_and_escrowed_bag_count_toward_three_percent() {
    let mut env = setup();
    let creator = user(&mut env.svm, 20);
    let l = create(&mut env, &creator, seed(51), 3 * SOL / 2, 0, 86_400).unwrap();
    let a = user(&mut env.svm, 20);
    let most = line_max(&env, &l);
    commit(&mut env, &l, &a, most).unwrap();
    advance(&mut env.svm, AUCTION + DECAY, 400);
    settle(&mut env, &l, &a).unwrap();
    // a hasn't claimed yet: another 3% on the curve is refused, a small top-up isn't
    let e = trade(&mut env, &l, &a, true, most).unwrap_err();
    assert!(e.contains("WalletCapExceeded"), "{e}");
    let s = launch_state(&env.svm, &l.launch);
    let share = s.batch_tokens; // a is the whole line
    let room = MAX_WALLET_TOKENS_BONDING - share;
    assert!(room < MAX_WALLET_TOKENS_BONDING / 10, "line share already near 3%: room {room}");
    claim(&mut env, &l, &a).unwrap();
    let e = trade(&mut env, &l, &a, true, most).unwrap_err();
    assert!(e.contains("WalletCapExceeded"), "{e}");
    // the creator's escrowed bag counts while it vests
    let bag = s.creator_tokens;
    let e = trade(&mut env, &l, &creator, true, 3 * SOL / 2).unwrap_err();
    assert!(e.contains("WalletCapExceeded"), "bag {bag} + new buy must stay under 3%: {e}");
    trade(&mut env, &l, &creator, true, SOL / 50).unwrap();
}

/// 70/30 curve: 700M tokens on the curve, 300M into the pool. With virtual tokens
/// VT = C^2 / (C - LP) the pool opens at the curve's last price, so bonding doesn't
/// jump the price. Pure config: no program change needed.
#[test]
fn seventy_thirty_curve_bonds_at_the_curve_price() {
    const CT: u64 = 700_000_000_000_000;
    const VT: u64 = 1_225_000_000_000_000; // 700^2 / (700 - 300) = 1225M
    // Small raise so the test can fill it: bonds at ~20 SOL (V0 = R (VT - C) / C).
    const RAISE: u64 = 20 * SOL;
    let mut env = setup_with(|p| { p.curve_tokens = CT; p.virtual_tokens = VT; p.virtual_sol = (RAISE as u128 * (VT - CT) as u128 / CT as u128) as u64; });
    let creator = user(&mut env.svm, 5);
    let l = create(&mut env, &creator, seed(49), 0, 0, 0).unwrap();
    let s = launch_state(&env.svm, &l.launch);
    assert_eq!((s.real_token, s.lp_reserve), (CT, TOTAL_SUPPLY - CT));
    let ws = fill_line(&mut env, &l, 80);
    advance(&mut env.svm, AUCTION, 20);
    settle(&mut env, &l, &ws[0]).unwrap();
    let s = launch_state(&env.svm, &l.launch);
    assert!(s.complete, "a full line of 3% wallets fills a 20 SOL curve");
    let end_price = s.virtual_sol as f64 / s.virtual_token as f64;
    graduate(&mut env, &l, &ws[1]).unwrap();
    let k = pool_keys(&env, &l);
    let (tv, sv) = if k.m0 == wsol() { (k.v1, k.v0) } else { (k.v0, k.v1) };
    assert_eq!(tok(&env.svm, &tv), TOTAL_SUPPLY - CT, "pool holds 30% of the supply");
    let pool_price = tok(&env.svm, &sv) as f64 / tok(&env.svm, &tv) as f64;
    let gap = (pool_price / end_price - 1.0).abs();
    println!("70/30: curve end price {end_price:.3e}, pool price {pool_price:.3e}, gap {:.2}%", gap * 100.0);
    // the pool only misses the curve price by the bonding fee and pool overhead
    assert!(pool_price <= end_price && gap < 0.08, "gap {gap}");
}

// ───────────────────────────── attack tests ─────────────────────────────

fn admin_ix(config: Pubkey, data: Vec<u8>, authority: &Pubkey) -> Instruction {
    Instruction { program_id: pid(), accounts: fairlaunch::accounts::AdminConfig { authority: *authority, config }.to_account_metas(None), data }
}
fn apply_ix(config: Pubkey) -> Instruction {
    Instruction { program_id: pid(), accounts: fairlaunch::accounts::ApplyConfig { config }.to_account_metas(None), data: fairlaunch::instruction::ApplyConfig {}.data() }
}
fn config_state(env: &Env) -> Config {
    Config::try_deserialize(&mut env.svm.get_account(&env.config).unwrap().data.as_slice()).unwrap()
}

#[test]
fn only_the_upgrade_authority_can_initialise_the_config() {
    let mut svm = LiteSVM::new();
    let admin = Keypair::new();
    deploy_upgradeable(&mut svm, &admin.pubkey());
    let squatter = Keypair::new();
    svm.airdrop(&squatter.pubkey(), 10 * SOL).unwrap();
    svm.airdrop(&admin.pubkey(), 10 * SOL).unwrap();
    let (config, _) = Pubkey::find_program_address(&[CONFIG_SEED], &pid());
    let p = params(squatter.pubkey(), squatter.pubkey());
    let e = send(&mut svm, vec![init_config_ix(config, &squatter.pubkey(), p.clone())], &squatter).unwrap_err();
    assert!(e.contains("InvalidConfig"), "{e}");
    send(&mut svm, vec![init_config_ix(config, &admin.pubkey(), p)], &admin).unwrap();
}

#[test]
fn config_changes_wait_for_the_timelock_and_raydium_is_fixed() {
    let mut env = setup();
    let admin = env.admin.insecure_clone();
    let mut p = params(env.treasury, env.distributor.pubkey());
    // fee caps hold even for the admin
    p.base_fee_bps = 500;
    let e = send(&mut env.svm, vec![admin_ix(env.config, fairlaunch::instruction::ProposeConfig { params: p.clone(), timelock_secs: TIMELOCK }.data(), &admin.pubkey())], &admin).unwrap_err();
    assert!(e.contains("InvalidConfig"), "{e}");
    // Raydium program can't be swapped for a malicious one
    let mut p = params(env.treasury, env.distributor.pubkey());
    p.cpmm_program = Keypair::new().pubkey();
    let e = send(&mut env.svm, vec![admin_ix(env.config, fairlaunch::instruction::ProposeConfig { params: p, timelock_secs: TIMELOCK }.data(), &admin.pubkey())], &admin).unwrap_err();
    assert!(e.contains("ImmutableField"), "{e}");
    // a stranger can't propose
    let stranger = user(&mut env.svm, 1);
    let mut p = params(stranger.pubkey(), env.distributor.pubkey());
    let e = send(&mut env.svm, vec![admin_ix(env.config, fairlaunch::instruction::ProposeConfig { params: p.clone(), timelock_secs: TIMELOCK }.data(), &stranger.pubkey())], &stranger).unwrap_err();
    assert!(e.contains("ConstraintHasOne") || e.contains("has one"), "{e}");
    // treasury change: queued, not live until the timelock passes
    p.treasury = Keypair::new().pubkey();
    send(&mut env.svm, vec![admin_ix(env.config, fairlaunch::instruction::ProposeConfig { params: p.clone(), timelock_secs: TIMELOCK }.data(), &admin.pubkey())], &admin).unwrap();
    assert_eq!(config_state(&env).treasury, env.treasury);
    let e = send(&mut env.svm, vec![apply_ix(env.config)], &stranger).unwrap_err();
    assert!(e.contains("TimelockNotReady"), "{e}");
    advance(&mut env.svm, 10, TIMELOCK + 1);
    send(&mut env.svm, vec![apply_ix(env.config)], &stranger).unwrap();
    assert_eq!(config_state(&env).treasury, p.treasury);
    let e = send(&mut env.svm, vec![apply_ix(env.config)], &stranger).unwrap_err();
    assert!(e.contains("NoPendingChange"), "{e}");
}

#[test]
fn authority_handover_is_two_step_and_brake_stops_payouts() {
    let mut env = setup();
    let admin = env.admin.insecure_clone();
    let multisig = user(&mut env.svm, 1);
    let stranger = user(&mut env.svm, 1);
    send(&mut env.svm, vec![admin_ix(env.config, fairlaunch::instruction::TransferAuthority { new_authority: multisig.pubkey() }.data(), &admin.pubkey())], &admin).unwrap();
    let accept = |k: &Keypair| Instruction { program_id: pid(), accounts: fairlaunch::accounts::AcceptAuthority { new_authority: k.pubkey(), config: env.config }.to_account_metas(None), data: fairlaunch::instruction::AcceptAuthority {}.data() };
    let (a1, a2) = (accept(&stranger), accept(&multisig));
    assert!(send(&mut env.svm, vec![a1], &stranger).unwrap_err().contains("NotPendingAuthority"));
    send(&mut env.svm, vec![a2], &multisig).unwrap();
    assert_eq!(config_state(&env).authority, multisig.pubkey());
    // old admin is out
    assert!(send(&mut env.svm, vec![admin_ix(env.config, fairlaunch::instruction::DisableDistributor {}.data(), &admin.pubkey())], &admin).is_err());

    // brake: payouts stop instantly
    let creator = user(&mut env.svm, 10);
    let l = create(&mut env, &creator, seed(20), SOL, 0, 0).unwrap();
    let a = user(&mut env.svm, 10);
    commit(&mut env, &l, &a, SOL).unwrap();
    advance(&mut env.svm, AUCTION, 20);
    settle(&mut env, &l, &a).unwrap();
    claim(&mut env, &l, &a).unwrap();
    send(&mut env.svm, vec![admin_ix(env.config, fairlaunch::instruction::DisableDistributor {}.data(), &multisig.pubkey())], &multisig).unwrap();
    let dist = env.distributor.insecure_clone();
    let e = distribute(&mut env, &l, &dist, &[(a.pubkey(), 1000)]).unwrap_err();
    assert!(e.contains("NotDistributor"), "{e}");
}

#[test]
fn stolen_distributor_key_cannot_drain_the_holder_pool() {
    let mut env = setup();
    let creator = user(&mut env.svm, 10);
    let l = create(&mut env, &creator, seed(21), 0, 0, 0).unwrap();
    let holder = user(&mut env.svm, 20);
    commit(&mut env, &l, &holder, LINE_MAX).unwrap();
    advance(&mut env.svm, AUCTION, 20);
    settle(&mut env, &l, &holder).unwrap();
    claim(&mut env, &l, &holder).unwrap();
    let flip = user(&mut env.svm, 20);
    trade(&mut env, &l, &flip, true, 2 * SOL).unwrap(); // big cover charge into the pool
    // the thief buys a sliver after the cover charge has decayed
    advance(&mut env.svm, DECAY + 1, 60);
    let thief = user(&mut env.svm, 5);
    trade(&mut env, &l, &thief, true, SOL / 100).unwrap();
    let pool = launch_state(&env.svm, &l.launch).reward_sol;
    let dist = env.distributor.insecure_clone();

    // whole pool to the thief: refused (way above 4x their share of supply)
    let e = distribute_epoch(&mut env, &l, &dist, 1, &[(thief.pubkey(), pool)]).unwrap_err();
    assert!(e.contains("RewardAboveShare"), "{e}");
    // the thief's fair-ish cap is tiny
    let s = launch_state(&env.svm, &l.launch);
    let mint = spl_token::state::Mint::unpack(&env.svm.get_account(&l.mint).unwrap().data).unwrap();
    let circ = mint.supply - tok(&env.svm, &l.vault) - tok(&env.svm, &l.escrow);
    let cap = (4u128 * tok(&env.svm, &ata(&thief.pubkey(), &l.mint)) as u128 * s.reward_sol as u128 / circ as u128) as u64;
    assert!(cap < pool / 50, "thief cap {cap} vs pool {pool}");
    distribute_epoch(&mut env, &l, &dist, 1, &[(thief.pubkey(), cap)]).unwrap();
    // replaying the same epoch can't exceed the epoch budget, and a new epoch has to wait
    let e = distribute_epoch(&mut env, &l, &dist, 2, &[(thief.pubkey(), 1)]).unwrap_err();
    assert!(e.contains("EpochTooSoon"), "{e}");
    let e = distribute_epoch(&mut env, &l, &dist, 0, &[(thief.pubkey(), 1)]).unwrap_err();
    assert!(e.contains("BadEpoch"), "{e}");
    // after the batch window, the old epoch is closed
    advance(&mut env.svm, 10, 1_000);
    let e = distribute_epoch(&mut env, &l, &dist, 1, &[(thief.pubkey(), 1)]).unwrap_err();
    assert!(e.contains("EpochWindowClosed"), "{e}");
    // a real holder with most of the supply can be paid most of the pool
    advance(&mut env.svm, 10, 3_000);
    let left = launch_state(&env.svm, &l.launch).reward_sol;
    distribute_epoch(&mut env, &l, &dist, 2, &[(holder.pubkey(), left * 9 / 10)]).unwrap();
    // paying a program account (the curve vault's owner) is refused
    advance(&mut env.svm, 10, 3_100);
    let e = distribute_epoch(&mut env, &l, &dist, 3, &[(l.launch, 1)]).unwrap_err();
    assert!(e.contains("BadRewardAccount"), "{e}");
}

#[test]
fn someone_creating_the_raydium_pool_first_cannot_block_graduation() {
    let mut env = setup();
    let creator = user(&mut env.svm, 10);
    let l = create(&mut env, &creator, seed(22), SOL, 0, 0).unwrap();
    let attacker = user(&mut env.svm, 20);
    let most = line_max(&env, &l);
    commit(&mut env, &l, &attacker, most).unwrap();
    let whales = fill_line(&mut env, &l, 59);
    advance(&mut env.svm, AUCTION, 20);
    settle(&mut env, &l, &attacker).unwrap();
    claim(&mut env, &l, &attacker).unwrap();
    assert!(launch_state(&env.svm, &l.launch).complete);

    // Attack: create the coin/WSOL pool at Raydium's predictable address first.
    let (m0, m1, token_is_1) = raydium::sorted(l.mint, wsol());
    let pda_pool = raydium::pool_state(&cpmm(), &amm_config(), &m0, &m1);
    let a = attacker.pubkey();
    let a_wsol = ata(&a, &wsol());
    let a_tok = ata(&a, &l.mint);
    let lp = raydium::lp_mint(&cpmm(), &pda_pool);
    let keys = raydium::InitializeKeys {
        program: cpmm(), creator: a, amm_config: amm_config(), authority: raydium::authority(&cpmm()), pool_state: pda_pool,
        token_0_mint: m0, token_1_mint: m1, lp_mint: lp,
        creator_token_0: if token_is_1 { a_wsol } else { a_tok }, creator_token_1: if token_is_1 { a_tok } else { a_wsol },
        creator_lp_token: ata(&a, &lp), token_0_vault: raydium::vault(&cpmm(), &pda_pool, &m0), token_1_vault: raydium::vault(&cpmm(), &pda_pool, &m1),
        create_pool_fee: pool_fee_rx(), observation_state: raydium::observation(&cpmm(), &pda_pool),
        token_program: spl_token::ID, associated_token_program: spl_associated_token_account::ID, system_program: system_program::ID, rent: sysvar::rent::ID,
    };
    let (amt0, amt1) = if token_is_1 { (SOL / 10, 1_000_000_000) } else { (1_000_000_000, SOL / 10) };
    let mut init = raydium::initialize_ix(&keys, amt0, amt1, 0);
    init.accounts[3].is_signer = false; // the PDA path
    let ixs = vec![
        spl_associated_token_account::instruction::create_associated_token_account_idempotent(&a, &a, &wsol(), &spl_token::ID),
        solana_sdk::system_instruction::transfer(&a, &a_wsol, SOL / 5),
        spl_token::instruction::sync_native(&spl_token::ID, &a_wsol).unwrap(),
        init,
    ];
    send(&mut env.svm, ixs, &attacker).expect("attacker can create the PDA pool");
    assert_eq!(env.svm.get_account(&pda_pool).unwrap().owner, cpmm());

    // Graduation still works: the real pool lives at a fresh address.
    graduate(&mut env, &l, &whales[0]).unwrap();
    let s = launch_state(&env.svm, &l.launch);
    assert!(s.graduated && s.pool != pda_pool);
    lock_liquidity(&mut env, &l, &whales[1]).unwrap();
    assert!(launch_state(&env.svm, &l.launch).lp_locked);
    // and it can't graduate twice
    assert!(graduate(&mut env, &l, &whales[2]).is_err());
}

// ───────────────────────────── fuzzing ─────────────────────────────
// Random sequences of every user-facing instruction against the real program (and real
// Raydium for graduation). After every step the books must balance; every instruction
// must succeed exactly when the rules say it should.

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 { self.0 ^= self.0 << 13; self.0 ^= self.0 >> 7; self.0 ^= self.0 << 17; self.0 }
    fn below(&mut self, n: u64) -> u64 { self.next() % n.max(1) }
    fn chance(&mut self, pct: u64) -> bool { self.below(100) < pct }
}

struct FL { l: L, creator: usize, commits: std::collections::HashMap<usize, u64>, claimed: std::collections::HashSet<usize>, last_rewards: u64 }

fn is_open(s: &Launch) -> bool { s.settled && !s.cancelled && !s.complete && !s.graduated }
fn funds_err(e: &str) -> bool { e.contains("insufficient") || e.contains("InsufficientFunds") || e.contains("0x1") }

fn check_books(env: &Env, f: &mut FL, ctx: &str) {
    let s = launch_state(&env.svm, &f.l.launch);
    let acc = env.svm.get_account(&f.l.launch).unwrap();
    let rent = env.svm.minimum_balance_for_rent_exemption(acc.data.len());
    assert!(acc.lamports >= rent + s.liabilities(), "[{ctx}] insolvent: lamports {} < rent {} + liabilities {}", acc.lamports, rent, s.liabilities());
    let vault = tok(&env.svm, &f.l.vault);
    let need = s.real_token + s.unclaimed_batch + if s.graduated { 0 } else { s.lp_reserve };
    assert!(vault >= need, "[{ctx}] vault {vault} < owed {need}");
    if !s.cancelled || s.creator_tokens > 0 {
        assert_eq!(tok(&env.svm, &f.l.escrow), s.creator_tokens - s.creator_claimed, "[{ctx}] escrow mismatch");
    }
    let mint = spl_token::state::Mint::unpack(&env.svm.get_account(&f.l.mint).unwrap().data).unwrap();
    assert!(mint.mint_authority.is_none() && mint.freeze_authority.is_none(), "[{ctx}] authority reappeared");
    assert_eq!(mint.supply, TOTAL_SUPPLY, "[{ctx}] supply changed");
    let total_rewards = s.reward_sol + s.reward_paid;
    assert!(total_rewards >= f.last_rewards, "[{ctx}] holder rewards went backwards");
    f.last_rewards = total_rewards;
    let committed: u64 = f.commits.values().sum();
    assert_eq!(committed, s.total_committed, "[{ctx}] committed total mismatch");
}

fn run_fuzz(seed: u64, steps: usize) -> (usize, usize, usize) {
    let mut env = setup();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ (seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1));
    let users: Vec<Keypair> = (0..24).map(|_| user(&mut env.svm, 60)).collect();
    let mut fl: Vec<FL> = vec![];
    let (mut ok, mut rejected, mut graduated) = (0usize, 0usize, 0usize);
    for step in 0..steps {
        let ctx = format!("seed {seed} step {step}");
        // Mostly pick actions that fit a random coin's stage, with some noise to test refusals.
        let mut op = rng.below(13);
        if !fl.is_empty() && rng.chance(75) {
            let probe = launch_state(&env.svm, &fl[rng.below(fl.len() as u64) as usize].l.launch);
            let now = slot(&env.svm);
            op = if !probe.settled && now < probe.auction_end_slot { [1, 1, 2, 3, 4][rng.below(5) as usize] }
                 else if !probe.settled { 5 }
                 else if probe.complete && !probe.graduated { 12 }
                 else if is_open(&probe) { [6, 6, 7, 8, 8, 9, 9, 10, 11, 4][rng.below(10) as usize] }
                 else { [6, 11, 4, 0][rng.below(4) as usize] };
        }
        if fl.is_empty() || (op == 0 && fl.len() < 3) {
            let c = rng.below(users.len() as u64) as usize;
            let buy = [0, SOL / 10, SOL, 3 * SOL / 2][rng.below(4) as usize];
            let min_raise = [0, 0, 5 * SOL, 60 * SOL][rng.below(4) as usize];
            let vest = [0i64, 600, 86_400][rng.below(3) as usize];
            let mut sd = [0u8; 16]; sd[..8].copy_from_slice(&seed.to_le_bytes()); sd[8] = fl.len() as u8; sd[9] = step as u8; sd[10] = (step >> 8) as u8;
            let l = create(&mut env, &users[c], sd, buy, min_raise, vest).unwrap_or_else(|e| panic!("[{ctx}] create failed: {e}"));
            fl.push(FL { l, creator: c, commits: Default::default(), claimed: Default::default(), last_rewards: 0 });
            ok += 1;
        } else {
            let i = rng.below(fl.len() as u64) as usize;
            let l = fl[i].l;
            let s = launch_state(&env.svm, &l.launch);
            let now_slot = slot(&env.svm);
            let mut u = rng.below(users.len() as u64) as usize;
            // claims: usually pick someone who is actually owed something
            if op == 6 && rng.chance(80) {
                let owed: Vec<usize> = fl[i].commits.keys().copied().filter(|x| !fl[i].claimed.contains(x)).collect();
                if !owed.is_empty() { u = owed[rng.below(owed.len() as u64) as usize]; }
            }
            match op {
                1 | 2 | 3 => {
                    // commit, sometimes over the cap
                    let amt = if rng.chance(10) { CAP + 1 } else if rng.chance(60) { (rng.below(30) + 20) * SOL / 10 } else { (rng.below(20) + 1) * SOL / 10 };
                    let prev = *fl[i].commits.get(&u).unwrap_or(&0);
                    let own = if u == fl[i].creator { s.creator_tokens } else { 0 };
                    let should = !s.settled && now_slot < s.auction_end_slot && prev + amt <= CAP && line_fits(&s, prev + amt, own);
                    match commit(&mut env, &l, &users[u], amt) {
                        Ok(()) => { assert!(should, "[{ctx}] commit accepted when it should fail"); *fl[i].commits.entry(u).or_default() += amt; ok += 1 }
                        Err(e) => { assert!(!should || funds_err(&e), "[{ctx}] commit refused when allowed: {e}"); rejected += 1 }
                    }
                }
                4 => advance(&mut env.svm, rng.below(25) + 1, rng.below(700) as i64 + 1),
                5 => {
                    let should = !s.settled && now_slot >= s.auction_end_slot;
                    match settle(&mut env, &l, &users[u]) {
                        Ok(()) => { assert!(should, "[{ctx}] settle accepted early/twice"); ok += 1 }
                        Err(e) => { assert!(!should, "[{ctx}] settle refused when due: {e}"); rejected += 1 }
                    }
                }
                6 => {
                    let has = fl[i].commits.contains_key(&u) && !fl[i].claimed.contains(&u);
                    let should = s.settled && has;
                    let before_t = tok(&env.svm, &ata(&users[u].pubkey(), &l.mint));
                    match claim(&mut env, &l, &users[u]) {
                        Ok(()) => {
                            assert!(should, "[{ctx}] claim accepted without a commitment / twice");
                            let s2 = launch_state(&env.svm, &l.launch);
                            let amt = fl[i].commits[&u] as u128;
                            let got = tok(&env.svm, &ata(&users[u].pubkey(), &l.mint)) - before_t;
                            let want = if s2.cancelled { 0 } else { (s2.batch_tokens as u128 * amt / s2.total_committed as u128) as u64 };
                            assert_eq!(got, want, "[{ctx}] unfair claim: got {got}, pro-rata {want}");
                            fl[i].claimed.insert(u); ok += 1
                        }
                        Err(e) => { assert!(!should, "[{ctx}] claim refused when owed: {e}"); rejected += 1 }
                    }
                }
                7 | 8 => {
                    let amt = if rng.chance(25) { (rng.below(15) + 5) * SOL } else { (rng.below(30) + 1) * SOL / 10 };
                    let should = is_open(&s);
                    match trade(&mut env, &l, &users[u], true, amt) {
                        Ok(()) => { assert!(should, "[{ctx}] buy accepted while curve closed (settled {} cancelled {} complete {})", s.settled, s.cancelled, s.complete); ok += 1 }
                        Err(e) => { assert!(!should || funds_err(&e), "[{ctx}] buy refused on open curve: {e}"); rejected += 1 }
                    }
                }
                9 => {
                    let bal_t = tok(&env.svm, &ata(&users[u].pubkey(), &l.mint));
                    let amt = if rng.chance(15) { bal_t + 1 } else { bal_t * (rng.below(100) + 1) / 100 };
                    let should = is_open(&s) && amt > 0 && amt <= bal_t;
                    match trade(&mut env, &l, &users[u], false, amt) {
                        Ok(()) => { assert!(should || amt == 0 && is_open(&s), "[{ctx}] sell accepted: amt {amt} bal {bal_t} open {}", is_open(&s)); ok += 1 }
                        Err(e) => { assert!(!should, "[{ctx}] sell refused: {e}"); rejected += 1 }
                    }
                }
                10 => {
                    let cr = fl[i].creator;
                    let r = send(&mut env.svm, vec![escrow_ix(&l, &users[cr], fairlaunch::instruction::ClaimVested {}.data())], &users[cr]);
                    if r.is_ok() { assert!(s.settled && !s.cancelled, "[{ctx}] vesting claim before settle / on cancelled"); ok += 1 } else { rejected += 1 }
                    // a stranger can never take the creator's bag
                    if u != cr { assert!(send(&mut env.svm, vec![escrow_ix(&l, &users[u], fairlaunch::instruction::ClaimVested {}.data())], &users[u]).is_err(), "[{ctx}] stranger claimed vesting"); }
                }
                11 => {
                    // distributor pays random wallets random amounts; the program must keep it in bounds
                    let dist = env.distributor.insecure_clone();
                    let n = rng.below(3) + 1;
                    let pays: Vec<(Pubkey, u64)> = (0..n).map(|_| { let w = users[rng.below(users.len() as u64) as usize].pubkey(); (w, rng.below(s.reward_sol.max(1) / 2 + 1)) }).collect();
                    let mut uniq = pays.clone(); uniq.sort(); uniq.dedup_by_key(|p| p.0);
                    let epoch = s.reward_epochs + if rng.chance(50) { 1 } else { 0 };
                    let before: Vec<u64> = uniq.iter().map(|p| bal(&env.svm, &p.0)).collect();
                    if distribute_epoch(&mut env, &l, &dist, epoch, &uniq).is_ok() {
                        let mint = spl_token::state::Mint::unpack(&env.svm.get_account(&l.mint).unwrap().data).unwrap();
                        let s2 = launch_state(&env.svm, &l.launch);
                        // same definition as the program: supply minus curve vault, escrow and (after graduation) the pool's token vault
                        let pool_tok = if s2.graduated { tok(&env.svm, &raydium::vault(&cpmm(), &s2.pool, &l.mint)) } else { 0 };
                        let circ = mint.supply - tok(&env.svm, &l.vault) - tok(&env.svm, &l.escrow) - pool_tok;
                        for ((w, amt), b) in uniq.iter().zip(before) {
                            let held = tok(&env.svm, &ata(w, &l.mint));
                            assert!(held > 0, "[{ctx}] paid a non-holder");
                            assert!((*amt as u128) * (circ as u128) <= 4 * held as u128 * s2.epoch_budget as u128, "[{ctx}] paid above 4x share");
                            assert_eq!(bal(&env.svm, w) - b, *amt);
                        }
                        assert!(s2.epoch_paid <= s2.epoch_budget, "[{ctx}] epoch over budget");
                        ok += 1
                    } else { rejected += 1 }
                }
                _ => {
                    if s.complete && !s.graduated {
                        graduate(&mut env, &l, &users[u]).unwrap_or_else(|e| panic!("[{ctx}] graduation failed: {e}"));
                        lock_liquidity(&mut env, &l, &users[u]).unwrap_or_else(|e| panic!("[{ctx}] lock failed: {e}"));
                        graduated += 1; ok += 1;
                    } else if !s.complete {
                        assert!(graduate(&mut env, &l, &users[u]).is_err(), "[{ctx}] graduated an incomplete curve");
                        rejected += 1;
                    }
                }
            }
        }
        for f in fl.iter_mut() { check_books(&env, f, &ctx); }
    }
    if std::env::var("FUZZ_DEBUG").is_ok() {
        for f in &fl { let s = launch_state(&env.svm, &f.l.launch); eprintln!("seed {seed}: real_sol {:.2} settled {} cancelled {} complete {} commits {}", s.real_sol as f64 / 1e9, s.settled, s.cancelled, s.complete, f.commits.len()); }
    }
    // Wrap up: open every door, everyone claims, cancelled lines refund in full.
    advance(&mut env.svm, 800, 3600);
    for f in fl.iter_mut() {
        let s = launch_state(&env.svm, &f.l.launch);
        if !s.settled { settle(&mut env, &f.l, &users[0]).unwrap(); }
        let pending: Vec<usize> = f.commits.keys().copied().filter(|u| !f.claimed.contains(u)).collect();
        for u in pending {
            let b = bal(&env.svm, &users[u].pubkey());
            claim(&mut env, &f.l, &users[u]).unwrap_or_else(|e| panic!("[seed {seed} wrap-up] claim failed: {e}"));
            if launch_state(&env.svm, &f.l.launch).cancelled {
                assert!(bal(&env.svm, &users[u].pubkey()) + 3_000_000 >= b + f.commits[&u], "[seed {seed}] refund short");
            }
            f.claimed.insert(u);
        }
        check_books(&env, f, &format!("seed {seed} wrap-up"));
        let s = launch_state(&env.svm, &f.l.launch);
        assert!(s.escrow_sol <= f.commits.len() as u64, "[seed {seed}] refund SOL left behind: {}", s.escrow_sol);
    }
    (ok, rejected, graduated)
}

#[test]
fn fuzz_random_sequences_keep_the_books_balanced() {
    let seeds: u64 = std::env::var("FUZZ_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(6);
    let steps: usize = std::env::var("FUZZ_STEPS").ok().and_then(|v| v.parse().ok()).unwrap_or(120);
    let (mut ok, mut rej, mut grad) = (0, 0, 0);
    for s in 0..seeds { let (a, b, c) = run_fuzz(s, steps); ok += a; rej += b; grad += c; }
    println!("fuzz: {seeds} seeds x {steps} steps: {ok} accepted, {rej} correctly refused, {grad} graduations");
}
