use anchor_lang::prelude::*;
use anchor_spl::{associated_token::AssociatedToken, token_interface::{Mint, TokenAccount, TokenInterface, mint_to_checked, MintToChecked}};
use mpl_core::{
    ID as MPL_CORE_ID,
    accounts::{BaseAssetV1, BaseCollectionV1},
    types::{UpdateAuthority, Attribute, Attributes, Plugin, PluginType, FreezeDelegate},
    instructions::{UpdatePluginV1CpiBuilder},
    fetch_plugin,
};
use crate::Config;
use crate::error::ErrorCode;
use crate::utils::{calculate_rewards, SECONDS_PER_DAY, update_attribute};

#[derive(Accounts)]
pub struct Unstake<'info> {
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
    #[account(address = Pubkey::from(MPL_CORE_ID.to_bytes()))]
    pub mpl_core_program: UncheckedAccount<'info>,
}
pub fn handler(ctx: Context<Unstake>) -> Result<()> {

    // We start by fetching the existing attributes
    let attributes_fetched: Option<Attributes> = fetch_plugin::<BaseAssetV1, Attributes>(
        &ctx.accounts.asset.to_account_info(),
        PluginType::Attributes,
    )
    .ok()
    .map(|(_, attrs, _)| attrs);

    // If the attributes don't exist, we return an error
    require!(attributes_fetched.is_some(), ErrorCode::AssetNotStaked);

    let attributes = attributes_fetched.unwrap();

    // Prepare the Attributes list to update based on the existing attributes
    let mut attributes_list: Vec<Attribute> = Vec::with_capacity(attributes.attribute_list.len());

    // Additional auxiliary variables
    let current_timestamp = Clock::get()?.unix_timestamp;
    let mut staked_timestamp: i64 = 0;
    let mut last_claimed_at: i64 = 0;
    let staked_time_days: u64;

    for attribute in &attributes.attribute_list {
        if attribute.key == "staked" {
            require!(attribute.value == "true", ErrorCode::AssetNotStaked);
        }
        else if attribute.key == "staked_at" {
            staked_timestamp = attribute.value.parse::<i64>().map_err(|_| ErrorCode::InvalidTimestamp)?;
            // Calculate the time (in seconds) since the asset was staked
            let total_staked_time_sec = current_timestamp.checked_sub(staked_timestamp).ok_or(ErrorCode::InvalidTimestamp)?;
            // Staked time in days for freeze period check
            let total_staked_time_days = total_staked_time_sec.checked_div(SECONDS_PER_DAY).ok_or(ErrorCode::InvalidTimestamp)?;
            require!(total_staked_time_days >= ctx.accounts.config.freeze_period as i64, ErrorCode::FreezePeriodNotElapsed);
        }
        else if attribute.key == "last_claimed_at" {
            last_claimed_at = attribute.value.parse::<i64>().map_err(|_| ErrorCode::InvalidTimestamp)?;
        }
        else {
            attributes_list.push(attribute.clone());
        }
    }

    // Fallback for assets staked before migration
    if last_claimed_at == 0 {
        last_claimed_at = staked_timestamp;
    }

    let reward_time_sec = current_timestamp.checked_sub(last_claimed_at).ok_or(ErrorCode::InvalidTimestamp)?;
    staked_time_days = (reward_time_sec as u64).checked_div(SECONDS_PER_DAY as u64).ok_or(ErrorCode::InvalidTimestamp)?;

    // Prepare signing seeds for the update authority
    let collection_key = ctx.accounts.collection.key();
    let signer_seeds = &[
        b"update_authority",
        collection_key.as_ref(),
        &[ctx.bumps.update_authority],
    ];

    // Now we update the asset Atributes Plugin (with the existing attributes, including the Staking attributes with reset values)

    // Add the Staking attributes first (reset values)
    attributes_list.push(Attribute {
        key: "staked".to_string(),
        value: "false".to_string(),
    });
    attributes_list.push(Attribute {
        key: "staked_at".to_string(),
        value: "0".to_string(),
    });
    attributes_list.push(Attribute {
        key: "last_claimed_at".to_string(),
        value: "0".to_string(),
    });

    UpdatePluginV1CpiBuilder::new(&ctx.accounts.mpl_core_program.to_account_info())
    .asset(&ctx.accounts.asset.to_account_info())
    .collection(Some(&ctx.accounts.collection.to_account_info()))
    .payer(&ctx.accounts.owner.to_account_info())
    .authority(Some(&ctx.accounts.update_authority.to_account_info()))
    .system_program(&ctx.accounts.system_program.to_account_info())
    .plugin(Plugin::Attributes(Attributes { attribute_list: attributes_list }))
    .invoke_signed(&[signer_seeds])?;

    // And we Thaw the asset (update the FreezeDelegate Plugin to false)
    UpdatePluginV1CpiBuilder::new(&ctx.accounts.mpl_core_program.to_account_info())
    .asset(&ctx.accounts.asset.to_account_info())
    .collection(Some(&ctx.accounts.collection.to_account_info()))
    .payer(&ctx.accounts.owner.to_account_info())
    .authority(Some(&ctx.accounts.update_authority.to_account_info()))
    .system_program(&ctx.accounts.system_program.to_account_info())
    .plugin(Plugin::FreezeDelegate(FreezeDelegate { frozen: false }))
    .invoke_signed(&[signer_seeds])?;

    // --- Challenge 2: Decrement Collection Stats ---

    // Fetch collection attributes
    let collection_attributes_fetched = fetch_plugin::<BaseCollectionV1, Attributes>(
        &ctx.accounts.collection.to_account_info(),
        PluginType::Attributes,
    ).ok().map(|(_, attrs, _)| attrs);

    if let Some(attrs) = collection_attributes_fetched {
        let mut coll_attr_list = attrs.attribute_list;
        let mut total_staked: u32 = 0;
        if let Some(attr) = coll_attr_list.iter().find(|a| a.key == "total_staked") {
            total_staked = attr.value.parse::<u32>().unwrap_or(0);
        }

        if total_staked > 0 {
            total_staked = total_staked.checked_sub(1).ok_or(ErrorCode::NumericalOverflow)?;
            update_attribute(&mut coll_attr_list, "total_staked", total_staked.to_string());

            UpdatePluginV1CpiBuilder::new(&ctx.accounts.mpl_core_program.to_account_info())
            .asset(&ctx.accounts.collection.to_account_info())
            .collection(None)
            .payer(&ctx.accounts.owner.to_account_info())
            .authority(Some(&ctx.accounts.update_authority.to_account_info()))
            .system_program(&ctx.accounts.system_program.to_account_info())
            .plugin(Plugin::Attributes(Attributes { attribute_list: coll_attr_list }))
            .invoke_signed(&[signer_seeds])?;
        }
    }

    // Finally, we want to mint rewards to the user

    // Calculate the amount
    let amount = calculate_rewards(
        staked_time_days,
        ctx.accounts.config.rewards_bps,
        ctx.accounts.rewards_mint.decimals,
    )?;

    if amount > 0 {
        // Prepare signer seeds for config PDA
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
    }

    Ok(())
}