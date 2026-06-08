use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::AssociatedToken,
    token_interface::{mint_to_checked, Mint, MintToChecked, TokenAccount, TokenInterface},
};
use mpl_core::{
    accounts::{BaseAssetV1, BaseCollectionV1},
    fetch_plugin,
    instructions::UpdatePluginV1CpiBuilder,
    types::{Attributes, Plugin, PluginType, UpdateAuthority},
    ID as MPL_CORE_ID,
};
use crate::state::Config;
use crate::error::ErrorCode;
use crate::utils::{calculate_rewards, SECONDS_PER_DAY, update_attribute};

#[derive(Accounts)]
pub struct ClaimRewards<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(
        seeds = [b"config", collection.key().as_ref()],
        bump = config.bump,
    )]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        has_one = owner @ ErrorCode::InvalidOwner,
        constraint = asset.update_authority == UpdateAuthority::Collection(collection.key()) @ ErrorCode::InvalidUpdateAuthority,
    )]
    pub asset: Account<'info, BaseAssetV1>,
    #[account(
        mut,
        has_one = update_authority @ ErrorCode::InvalidUpdateAuthority
    )]
    pub collection: Account<'info, BaseCollectionV1>,
    /// CHECK: This account data is not used, we only verify the address
    #[account(
        seeds = [b"update_authority", collection.key().as_ref()],
        bump,
    )]
    pub update_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        seeds = [b"rewards_mint", config.key().as_ref()],
        bump = config.rewards_bump,
    )]
    pub rewards_mint: InterfaceAccount<'info, Mint>,
    #[account(
        init_if_needed,
        payer = owner,
        associated_token::mint = rewards_mint,
        associated_token::authority = owner,
    )]
    pub user_rewards_ata: InterfaceAccount<'info, TokenAccount>,
    pub token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    /// CHECK: This is the MPL Core program
    #[account(address = MPL_CORE_ID)]
    pub mpl_core_program: UncheckedAccount<'info>,
}

pub fn handler(ctx: Context<ClaimRewards>) -> Result<()> {
    // Fetch asset attributes
    let attributes_fetched: Option<Attributes> = fetch_plugin::<BaseAssetV1, Attributes>(
        &ctx.accounts.asset.to_account_info(),
        PluginType::Attributes,
    )
    .ok()
    .map(|(_, attrs, _)| attrs);

    require!(attributes_fetched.is_some(), ErrorCode::AssetNotStaked);
    let attributes = attributes_fetched.unwrap();

    let mut staked = false;
    let mut staked_at: i64 = 0;
    let mut last_claimed_at: i64 = 0;
    let current_timestamp = Clock::get()?.unix_timestamp;

    for attribute in &attributes.attribute_list {
        if attribute.key == "staked" {
            staked = attribute.value == "true";
        } else if attribute.key == "staked_at" {
            staked_at = attribute.value.parse::<i64>().map_err(|_| ErrorCode::InvalidTimestamp)?;
        } else if attribute.key == "last_claimed_at" {
            last_claimed_at = attribute.value.parse::<i64>().map_err(|_| ErrorCode::InvalidTimestamp)?;
        }
    }

    require!(staked, ErrorCode::AssetNotStaked);
    
    // Fallback for assets staked before the migration
    if last_claimed_at == 0 {
        last_claimed_at = staked_at;
    }
    
    require!(last_claimed_at > 0, ErrorCode::InvalidTimestamp);

    // Calculate rewards
    let staked_time_sec = current_timestamp.checked_sub(last_claimed_at).ok_or(ErrorCode::NumericalOverflow)?;
    let staked_time_days = (staked_time_sec as u64).checked_div(SECONDS_PER_DAY as u64).ok_or(ErrorCode::NumericalOverflow)?;

    if staked_time_days > 0 {
        let amount = calculate_rewards(
            staked_time_days,
            ctx.accounts.config.rewards_bps,
            ctx.accounts.rewards_mint.decimals,
        )?;

        // Mint rewards
        let collection_key = ctx.accounts.collection.key();
        let config_seeds = &[
            b"config",
            collection_key.as_ref(),
            &[ctx.accounts.config.bump],
        ];
        let config_signer_seeds = &[&config_seeds[..]];

        mint_to_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                MintToChecked {
                    mint: ctx.accounts.rewards_mint.to_account_info(),
                    to: ctx.accounts.user_rewards_ata.to_account_info(),
                    authority: ctx.accounts.config.to_account_info(),
                },
                config_signer_seeds,
            ),
            amount,
            ctx.accounts.rewards_mint.decimals,
        )?;

        // Update last_claimed_at to current_timestamp
        // We only update it if some days have passed and rewards were minted, 
        // OR we update it anyway but that might be better to keep track of fractional days?
        // The current logic only gives rewards for FULL days.
        // To be fair, we should only update last_claimed_at by the number of full days we paid for.
        
        let paid_time_sec = staked_time_days.checked_mul(SECONDS_PER_DAY as u64).ok_or(ErrorCode::NumericalOverflow)?;
        let new_last_claimed_at = last_claimed_at.checked_add(paid_time_sec as i64).ok_or(ErrorCode::NumericalOverflow)?;

        let mut new_attributes_list = attributes.attribute_list.clone();
        update_attribute(&mut new_attributes_list, "last_claimed_at", new_last_claimed_at.to_string());

        let signer_seeds = &[
            b"update_authority",
            collection_key.as_ref(),
            &[ctx.bumps.update_authority],
        ];

        UpdatePluginV1CpiBuilder::new(&ctx.accounts.mpl_core_program.to_account_info())
            .asset(&ctx.accounts.asset.to_account_info())
            .collection(Some(&ctx.accounts.collection.to_account_info()))
            .payer(&ctx.accounts.owner.to_account_info())
            .authority(Some(&ctx.accounts.update_authority.to_account_info()))
            .system_program(&ctx.accounts.system_program.to_account_info())
            .plugin(Plugin::Attributes(Attributes { attribute_list: new_attributes_list }))
            .invoke_signed(&[signer_seeds])?;
    }

    Ok(())
}
