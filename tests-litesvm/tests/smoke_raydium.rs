use litesvm::LiteSVM;
use solana_sdk::{instruction::Instruction, pubkey::Pubkey, signature::Keypair, signer::Signer, transaction::Transaction};
use std::str::FromStr;
#[test]
fn raydium_program_loads_and_runs() {
    let mut svm = LiteSVM::new();
    let id = Pubkey::from_str("DRaycpLY18LhpbydsBWbVJtxpNv9oXPgjRSfpF2bWpYb").unwrap();
    svm.add_program_from_file(id, "fixtures/raydium_cpmm.so").unwrap();
    let mpl = Pubkey::from_str("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s").unwrap();
    svm.add_program_from_file(mpl, "fixtures/mpl_token_metadata.so").unwrap();
    let p = Keypair::new(); svm.airdrop(&p.pubkey(), 1_000_000_000).unwrap();
    let ix = Instruction { program_id: id, accounts: vec![], data: vec![0u8; 8] };
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&p.pubkey()), &[&p], svm.latest_blockhash());
    let r = svm.send_transaction(tx);
    let logs = match &r { Ok(m) => m.logs.clone(), Err(e) => e.meta.logs.clone() };
    println!("{:#?}", logs);
    assert!(logs.iter().any(|l| l.contains("InstructionFallbackNotFound") || l.contains("Error Code")), "program did not execute Anchor code");
}
