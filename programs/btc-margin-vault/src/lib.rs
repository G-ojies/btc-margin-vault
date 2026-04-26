use anchor_lang::prelude::*;
use ika_dwallet_anchor::{DWalletContext, CPI_AUTHORITY_SEED};

pub use ika_dwallet_anchor::CPI_AUTHORITY_SEED as IKA_CPI_AUTHORITY_SEED;

declare_id!("6UVY7nskmCBaeKS22E6oh5CkS9TcdcmazHzLAme4YNfB");

pub const LTV_THRESHOLD: u64 = 70;
pub const LIQUIDATION_THRESHOLD: u64 = 85;
pub const VAULT_SEED: &[u8] = b"vault";

#[program]
pub mod btc_margin_vault {
    use super::*;

    pub fn initialize_vault(ctx: Context<InitializeVault>, dwallet_id: Pubkey) -> Result<()> {
        let vault = &mut ctx.accounts.vault;
        vault.owner = ctx.accounts.owner.key();
        vault.dwallet_id = dwallet_id;
        vault.btc_collateral_usd = 0;
        vault.usdc_borrowed = 0;
        vault.is_active = true;
        Ok(())
    }

    pub fn register_dwallet(ctx: Context<UpdateVault>, dwallet_id: Pubkey) -> Result<()> {
        let vault = &mut ctx.accounts.vault;
        require!(vault.is_active, VaultError::VaultNotActive);
        vault.dwallet_id = dwallet_id;
        Ok(())
    }

    pub fn deposit_collateral(ctx: Context<UpdateVault>, amount_usd: u64) -> Result<()> {
        let vault = &mut ctx.accounts.vault;
        require!(vault.is_active, VaultError::VaultNotActive);
        vault.btc_collateral_usd = vault
            .btc_collateral_usd
            .checked_add(amount_usd)
            .ok_or(VaultError::MathOverflow)?;
        Ok(())
    }

    pub fn borrow_usdc(ctx: Context<UpdateVault>, amount: u64) -> Result<()> {
        let vault = &mut ctx.accounts.vault;
        require!(vault.is_active, VaultError::VaultNotActive);
        require!(
            vault.btc_collateral_usd > 0,
            VaultError::InsufficientCollateral
        );

        let new_borrowed = vault
            .usdc_borrowed
            .checked_add(amount)
            .ok_or(VaultError::MathOverflow)?;

        let ltv_numerator = new_borrowed
            .checked_mul(100)
            .ok_or(VaultError::MathOverflow)?;
        let new_ltv = ltv_numerator / vault.btc_collateral_usd;

        require!(
            new_ltv <= LTV_THRESHOLD,
            VaultError::InsufficientCollateral
        );

        vault.usdc_borrowed = new_borrowed;
        Ok(())
    }

    pub fn repay_usdc(ctx: Context<UpdateVault>, amount: u64) -> Result<()> {
        let vault = &mut ctx.accounts.vault;
        require!(vault.is_active, VaultError::VaultNotActive);
        vault.usdc_borrowed = vault.usdc_borrowed.saturating_sub(amount);
        Ok(())
    }

    pub fn liquidate(
        ctx: Context<Liquidate>,
        vault: Pubkey,
        message_digest: [u8; 32],
        message_metadata_digest: [u8; 32],
        user_pubkey: [u8; 32],
        signature_scheme: u16,
        message_approval_bump: u8,
    ) -> Result<()> {
        require_keys_eq!(ctx.accounts.vault.key(), vault, VaultError::VaultMismatch);

        {
            let v = &ctx.accounts.vault;
            require!(v.is_active, VaultError::VaultNotActive);
            require!(
                v.btc_collateral_usd > 0,
                VaultError::BelowLiquidationThreshold
            );
            let ltv_numerator = v
                .usdc_borrowed
                .checked_mul(100)
                .ok_or(VaultError::MathOverflow)?;
            let ltv = ltv_numerator / v.btc_collateral_usd;
            require!(
                ltv > LIQUIDATION_THRESHOLD,
                VaultError::BelowLiquidationThreshold
            );
        }

        let cpi = DWalletContext {
            dwallet_program: ctx.accounts.dwallet_program.to_account_info(),
            cpi_authority: ctx.accounts.cpi_authority.to_account_info(),
            caller_program: ctx.accounts.caller_program.to_account_info(),
            cpi_authority_bump: ctx.bumps.cpi_authority,
        };
        cpi.approve_message(
            &ctx.accounts.dwallet_coordinator,
            &ctx.accounts.message_approval,
            &ctx.accounts.dwallet,
            &ctx.accounts.liquidator.to_account_info(),
            &ctx.accounts.system_program.to_account_info(),
            message_digest,
            message_metadata_digest,
            user_pubkey,
            signature_scheme,
            message_approval_bump,
        )?;

        let v = &mut ctx.accounts.vault;
        v.is_active = false;
        v.btc_collateral_usd = 0;
        v.usdc_borrowed = 0;
        Ok(())
    }
}

#[account]
#[derive(InitSpace)]
pub struct VaultState {
    pub owner: Pubkey,
    pub dwallet_id: Pubkey,
    pub btc_collateral_usd: u64,
    pub usdc_borrowed: u64,
    pub is_active: bool,
}

#[derive(Accounts)]
pub struct InitializeVault<'info> {
    #[account(
        init,
        payer = owner,
        space = 8 + VaultState::INIT_SPACE,
        seeds = [VAULT_SEED, owner.key().as_ref()],
        bump,
    )]
    pub vault: Account<'info, VaultState>,
    #[account(mut)]
    pub owner: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateVault<'info> {
    #[account(
        mut,
        seeds = [VAULT_SEED, owner.key().as_ref()],
        bump,
        has_one = owner,
    )]
    pub vault: Account<'info, VaultState>,
    pub owner: Signer<'info>,
}

#[derive(Accounts)]
pub struct Liquidate<'info> {
    #[account(
        mut,
        constraint = vault.dwallet_id == dwallet.key() @ VaultError::VaultMismatch,
    )]
    pub vault: Account<'info, VaultState>,

    #[account(mut)]
    pub liquidator: Signer<'info>,

    /// CHECK: Ika dWallet program; the runtime CPI invocation validates it.
    pub dwallet_program: UncheckedAccount<'info>,

    /// CHECK: PDA[CPI_AUTHORITY_SEED] of this program; signs the CPI via seeds.
    #[account(seeds = [CPI_AUTHORITY_SEED], bump)]
    pub cpi_authority: UncheckedAccount<'info>,

    /// CHECK: this program's own account, bound to declare_id below.
    #[account(address = crate::ID)]
    pub caller_program: UncheckedAccount<'info>,

    /// CHECK: Ika DWalletCoordinator PDA; validated by the Ika program.
    pub dwallet_coordinator: UncheckedAccount<'info>,

    /// CHECK: dWallet account; the vault constraint binds this to vault.dwallet_id.
    pub dwallet: UncheckedAccount<'info>,

    /// CHECK: MessageApproval PDA, created by the Ika program via CPI.
    #[account(mut)]
    pub message_approval: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

#[error_code]
pub enum VaultError {
    #[msg("Borrow would exceed maximum LTV; insufficient collateral")]
    InsufficientCollateral,
    #[msg("Vault is below the liquidation threshold and cannot be liquidated")]
    BelowLiquidationThreshold,
    #[msg("Vault is not active")]
    VaultNotActive,
    #[msg("Provided vault key does not match the vault account")]
    VaultMismatch,
    #[msg("Math overflow")]
    MathOverflow,
}
