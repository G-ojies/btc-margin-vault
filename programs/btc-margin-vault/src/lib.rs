use anchor_lang::prelude::*;

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

    pub fn liquidate(ctx: Context<Liquidate>, vault: Pubkey) -> Result<()> {
        require_keys_eq!(ctx.accounts.vault.key(), vault, VaultError::VaultMismatch);

        let vault_account = &mut ctx.accounts.vault;
        require!(vault_account.is_active, VaultError::VaultNotActive);
        require!(
            vault_account.btc_collateral_usd > 0,
            VaultError::BelowLiquidationThreshold
        );

        let ltv_numerator = vault_account
            .usdc_borrowed
            .checked_mul(100)
            .ok_or(VaultError::MathOverflow)?;
        let ltv = ltv_numerator / vault_account.btc_collateral_usd;

        require!(
            ltv > LIQUIDATION_THRESHOLD,
            VaultError::BelowLiquidationThreshold
        );

        vault_account.is_active = false;
        vault_account.btc_collateral_usd = 0;
        vault_account.usdc_borrowed = 0;
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
    #[account(mut)]
    pub vault: Account<'info, VaultState>,
    pub liquidator: Signer<'info>,
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
