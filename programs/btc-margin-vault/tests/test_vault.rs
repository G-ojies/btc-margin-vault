use {
    anchor_lang::{
        solana_program::instruction::Instruction, AccountDeserialize, AnchorSerialize,
        Discriminator, InstructionData, ToAccountMetas,
    },
    btc_margin_vault::{VaultState, VAULT_SEED},
    litesvm::LiteSVM,
    solana_account::Account as SolanaAccount,
    solana_keypair::Keypair,
    solana_message::{Message, VersionedMessage},
    solana_pubkey::Pubkey,
    solana_signer::Signer,
    solana_transaction::versioned::VersionedTransaction,
};

const PROGRAM_SO: &[u8] = include_bytes!(concat!(
    env!("CARGO_TARGET_TMPDIR"),
    "/../deploy/btc_margin_vault.so"
));

const SYSTEM_PROGRAM_ID: Pubkey = Pubkey::new_from_array([0u8; 32]);

fn setup() -> (LiteSVM, Keypair, Pubkey, Pubkey) {
    let program_id = btc_margin_vault::id();
    let mut svm = LiteSVM::new();
    svm.add_program(program_id, PROGRAM_SO).unwrap();
    let owner = Keypair::new();
    svm.airdrop(&owner.pubkey(), 10_000_000_000).unwrap();
    let (vault_pda, _bump) =
        Pubkey::find_program_address(&[VAULT_SEED, owner.pubkey().as_ref()], &program_id);
    (svm, owner, vault_pda, program_id)
}

fn submit(svm: &mut LiteSVM, payer: &Keypair, ix: Instruction) -> Result<(), String> {
    let blockhash = svm.latest_blockhash();
    let msg = Message::new_with_blockhash(&[ix], Some(&payer.pubkey()), &blockhash);
    let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &[payer]).unwrap();
    svm.send_transaction(tx).map(|_| ()).map_err(|e| format!("{:?}", e))
}

fn read_vault(svm: &LiteSVM, vault_pda: &Pubkey) -> VaultState {
    let acct = svm.get_account(vault_pda).unwrap();
    VaultState::try_deserialize(&mut acct.data.as_slice()).unwrap()
}

fn init_ix(program_id: Pubkey, owner: Pubkey, vault_pda: Pubkey, dwallet_id: Pubkey) -> Instruction {
    Instruction::new_with_bytes(
        program_id,
        &btc_margin_vault::instruction::InitializeVault { dwallet_id }.data(),
        btc_margin_vault::accounts::InitializeVault {
            vault: vault_pda,
            owner,
            system_program: SYSTEM_PROGRAM_ID,
        }
        .to_account_metas(None),
    )
}

fn deposit_ix(program_id: Pubkey, owner: Pubkey, vault_pda: Pubkey, amount_usd: u64) -> Instruction {
    Instruction::new_with_bytes(
        program_id,
        &btc_margin_vault::instruction::DepositCollateral { amount_usd }.data(),
        btc_margin_vault::accounts::UpdateVault {
            vault: vault_pda,
            owner,
        }
        .to_account_metas(None),
    )
}

fn borrow_ix(program_id: Pubkey, owner: Pubkey, vault_pda: Pubkey, amount: u64) -> Instruction {
    Instruction::new_with_bytes(
        program_id,
        &btc_margin_vault::instruction::BorrowUsdc { amount }.data(),
        btc_margin_vault::accounts::UpdateVault {
            vault: vault_pda,
            owner,
        }
        .to_account_metas(None),
    )
}

fn repay_ix(program_id: Pubkey, owner: Pubkey, vault_pda: Pubkey, amount: u64) -> Instruction {
    Instruction::new_with_bytes(
        program_id,
        &btc_margin_vault::instruction::RepayUsdc { amount }.data(),
        btc_margin_vault::accounts::UpdateVault {
            vault: vault_pda,
            owner,
        }
        .to_account_metas(None),
    )
}

fn liquidate_ix(program_id: Pubkey, liquidator: Pubkey, vault_pda: Pubkey) -> Instruction {
    Instruction::new_with_bytes(
        program_id,
        &btc_margin_vault::instruction::Liquidate { vault: vault_pda }.data(),
        btc_margin_vault::accounts::Liquidate {
            vault: vault_pda,
            liquidator,
        }
        .to_account_metas(None),
    )
}

#[test]
fn initialize_vault_sets_initial_state() {
    let (mut svm, owner, vault_pda, program_id) = setup();
    let dwallet_id = Pubkey::new_unique();

    submit(
        &mut svm,
        &owner,
        init_ix(program_id, owner.pubkey(), vault_pda, dwallet_id),
    )
    .unwrap();

    let vault = read_vault(&svm, &vault_pda);
    assert_eq!(vault.owner, owner.pubkey());
    assert_eq!(vault.dwallet_id, dwallet_id);
    assert_eq!(vault.btc_collateral_usd, 0);
    assert_eq!(vault.usdc_borrowed, 0);
    assert!(vault.is_active);
}

#[test]
fn deposit_collateral_accumulates() {
    let (mut svm, owner, vault_pda, program_id) = setup();
    submit(
        &mut svm,
        &owner,
        init_ix(program_id, owner.pubkey(), vault_pda, Pubkey::new_unique()),
    )
    .unwrap();

    submit(
        &mut svm,
        &owner,
        deposit_ix(program_id, owner.pubkey(), vault_pda, 100_000),
    )
    .unwrap();
    submit(
        &mut svm,
        &owner,
        deposit_ix(program_id, owner.pubkey(), vault_pda, 50_000),
    )
    .unwrap();

    let vault = read_vault(&svm, &vault_pda);
    assert_eq!(vault.btc_collateral_usd, 150_000);
}

#[test]
fn borrow_at_max_ltv_succeeds() {
    let (mut svm, owner, vault_pda, program_id) = setup();
    submit(
        &mut svm,
        &owner,
        init_ix(program_id, owner.pubkey(), vault_pda, Pubkey::new_unique()),
    )
    .unwrap();
    submit(
        &mut svm,
        &owner,
        deposit_ix(program_id, owner.pubkey(), vault_pda, 100_000),
    )
    .unwrap();

    submit(
        &mut svm,
        &owner,
        borrow_ix(program_id, owner.pubkey(), vault_pda, 70_000),
    )
    .unwrap();

    let vault = read_vault(&svm, &vault_pda);
    assert_eq!(vault.usdc_borrowed, 70_000);
}

#[test]
fn borrow_above_ltv_fails() {
    let (mut svm, owner, vault_pda, program_id) = setup();
    submit(
        &mut svm,
        &owner,
        init_ix(program_id, owner.pubkey(), vault_pda, Pubkey::new_unique()),
    )
    .unwrap();
    submit(
        &mut svm,
        &owner,
        deposit_ix(program_id, owner.pubkey(), vault_pda, 100_000),
    )
    .unwrap();

    let res = submit(
        &mut svm,
        &owner,
        borrow_ix(program_id, owner.pubkey(), vault_pda, 80_000),
    );
    assert!(res.is_err(), "borrow above 70% LTV should fail");

    let vault = read_vault(&svm, &vault_pda);
    assert_eq!(vault.usdc_borrowed, 0);
}

#[test]
fn repay_reduces_debt() {
    let (mut svm, owner, vault_pda, program_id) = setup();
    submit(
        &mut svm,
        &owner,
        init_ix(program_id, owner.pubkey(), vault_pda, Pubkey::new_unique()),
    )
    .unwrap();
    submit(
        &mut svm,
        &owner,
        deposit_ix(program_id, owner.pubkey(), vault_pda, 100_000),
    )
    .unwrap();
    submit(
        &mut svm,
        &owner,
        borrow_ix(program_id, owner.pubkey(), vault_pda, 70_000),
    )
    .unwrap();

    submit(
        &mut svm,
        &owner,
        repay_ix(program_id, owner.pubkey(), vault_pda, 30_000),
    )
    .unwrap();

    let vault = read_vault(&svm, &vault_pda);
    assert_eq!(vault.usdc_borrowed, 40_000);
}

#[test]
fn liquidate_below_threshold_fails() {
    let (mut svm, owner, vault_pda, program_id) = setup();
    submit(
        &mut svm,
        &owner,
        init_ix(program_id, owner.pubkey(), vault_pda, Pubkey::new_unique()),
    )
    .unwrap();
    submit(
        &mut svm,
        &owner,
        deposit_ix(program_id, owner.pubkey(), vault_pda, 100_000),
    )
    .unwrap();
    submit(
        &mut svm,
        &owner,
        borrow_ix(program_id, owner.pubkey(), vault_pda, 70_000),
    )
    .unwrap();

    let liquidator = Keypair::new();
    svm.airdrop(&liquidator.pubkey(), 1_000_000_000).unwrap();

    let res = submit(
        &mut svm,
        &liquidator,
        liquidate_ix(program_id, liquidator.pubkey(), vault_pda),
    );
    assert!(res.is_err(), "liquidation at 70% LTV should fail");

    let vault = read_vault(&svm, &vault_pda);
    assert!(vault.is_active, "vault should remain active");
}

#[test]
fn liquidate_above_threshold_succeeds() {
    let (mut svm, owner, vault_pda, program_id) = setup();
    submit(
        &mut svm,
        &owner,
        init_ix(program_id, owner.pubkey(), vault_pda, Pubkey::new_unique()),
    )
    .unwrap();
    submit(
        &mut svm,
        &owner,
        deposit_ix(program_id, owner.pubkey(), vault_pda, 100_000),
    )
    .unwrap();
    submit(
        &mut svm,
        &owner,
        borrow_ix(program_id, owner.pubkey(), vault_pda, 70_000),
    )
    .unwrap();

    // Force LTV above 85% by simulating a price drop on the BTC collateral.
    // 70_000 borrowed against 80_000 collateral = 87.5% LTV.
    let underwater = VaultState {
        owner: owner.pubkey(),
        dwallet_id: Pubkey::new_unique(),
        btc_collateral_usd: 80_000,
        usdc_borrowed: 70_000,
        is_active: true,
    };
    let mut data = VaultState::DISCRIMINATOR.to_vec();
    underwater.serialize(&mut data).unwrap();

    let existing = svm.get_account(&vault_pda).unwrap();
    svm.set_account(
        vault_pda,
        SolanaAccount {
            lamports: existing.lamports,
            data,
            owner: existing.owner,
            executable: existing.executable,
            rent_epoch: existing.rent_epoch,
        },
    )
    .unwrap();

    let liquidator = Keypair::new();
    svm.airdrop(&liquidator.pubkey(), 1_000_000_000).unwrap();
    submit(
        &mut svm,
        &liquidator,
        liquidate_ix(program_id, liquidator.pubkey(), vault_pda),
    )
    .unwrap();

    let vault = read_vault(&svm, &vault_pda);
    assert!(!vault.is_active);
    assert_eq!(vault.btc_collateral_usd, 0);
    assert_eq!(vault.usdc_borrowed, 0);
}

#[test]
fn instruction_on_inactive_vault_fails() {
    let (mut svm, owner, vault_pda, program_id) = setup();
    submit(
        &mut svm,
        &owner,
        init_ix(program_id, owner.pubkey(), vault_pda, Pubkey::new_unique()),
    )
    .unwrap();

    let inactive = VaultState {
        owner: owner.pubkey(),
        dwallet_id: Pubkey::new_unique(),
        btc_collateral_usd: 0,
        usdc_borrowed: 0,
        is_active: false,
    };
    let mut data = VaultState::DISCRIMINATOR.to_vec();
    inactive.serialize(&mut data).unwrap();
    let existing = svm.get_account(&vault_pda).unwrap();
    svm.set_account(
        vault_pda,
        SolanaAccount {
            lamports: existing.lamports,
            data,
            owner: existing.owner,
            executable: existing.executable,
            rent_epoch: existing.rent_epoch,
        },
    )
    .unwrap();

    let res = submit(
        &mut svm,
        &owner,
        deposit_ix(program_id, owner.pubkey(), vault_pda, 100),
    );
    assert!(res.is_err(), "deposit on inactive vault should fail");
}
