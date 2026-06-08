use anchor_lang::prelude::*;
use mpl_core::{
    ID as MPL_CORE_ID,
    accounts::{BaseAssetV1, BaseCollectionV1},
    instructions::{AddPluginV1CpiBuilder, UpdatePluginV1CpiBuilder},
    types::{UpdateAuthority, Attribute, Attributes, Plugin, PluginAuthority, PluginType, FreezeDelegate},
    fetch_plugin,
};
use crate::state::Config;
use crate::error::ErrorCode;
use crate::utils::update_attribute;

#[derive(Accounts)]
pub struct Stake<'info> {
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
        has_one = update_authority @ ErrorCode::InvalidUpdateAuthority,
    )]
    pub collection: Account<'info, BaseCollectionV1>,
    /// CHECK: This account is not initialized and is being used for signing purposes only, we verify that derives from the correct seeds
    #[account(
        seeds = [b"update_authority", collection.key().as_ref()],
        bump,
    )]
    pub update_authority: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
    /// CHECK: This is the ID of the MPL Core Program
    #[account(address = MPL_CORE_ID)]
    pub mpl_core_program: UncheckedAccount<'info>,
}
pub fn handler(ctx: Context<Stake>) -> Result<()> {

    // We start by fetching the existing attributes (if they exist)
    let attributes_fetched: Option<Attributes> = fetch_plugin::<BaseAssetV1, Attributes>(
        &ctx.accounts.asset.to_account_info(), 
        PluginType::Attributes,
    )
    .ok()
    .map(|(_,attrs,_)| attrs);

    
    // Prepare the Attributes list to add or update based on the existing attributes
    let mut attributes_list: Vec<Attribute> = Vec::new();

    // Loop to all attributes and save only the ones that are not the Staking attributes ("staked", "staked_at", "last_claimed_at")
    // If we find the "staked" attribute already present, we need to make sure the asset is not already staked
    if let Some(attributes) = &attributes_fetched {
        for attribute in &attributes.attribute_list {
            if attribute.key == "staked" {
                require!(attribute.value == "false", ErrorCode::AlreadyStaked);
            }
            else if attribute.key != "staked_at" && attribute.key != "last_claimed_at" {
                attributes_list.push(attribute.clone());
            }
        }
    }

    let current_time = Clock::get()?.unix_timestamp;

    // Add the Staking attributes
    attributes_list.push(Attribute {
        key: "staked".to_string(),
        value: "true".to_string(),
    });
    attributes_list.push(Attribute {
        key: "staked_at".to_string(),
        value: current_time.to_string(),
    });
    attributes_list.push(Attribute {
        key: "last_claimed_at".to_string(),
        value: current_time.to_string(),
    });

    // Now that we have the complete list of Attributes we either add the Plugin or Update the existing one
    // The Attributes Plugin is an Authority-Managed Plugin, so it needs to be signed by the update authority (PDA of the program)

    // Prepare signing seeds for the update authority
    let collection_key = ctx.accounts.collection.key();
    let signer_seeds = &[
        b"update_authority",
        collection_key.as_ref(),
        &[ctx.bumps.update_authority],
    ];

    // If the Attributes Plugin does not exist, we add it
    if attributes_fetched.is_none() {
        AddPluginV1CpiBuilder::new(&ctx.accounts.mpl_core_program.to_account_info())
        .asset(&ctx.accounts.asset.to_account_info())
        .collection(Some(&ctx.accounts.collection.to_account_info()))
        .payer(&ctx.accounts.owner.to_account_info())
        .authority(Some(&ctx.accounts.update_authority.to_account_info()))
        .system_program(&ctx.accounts.system_program.to_account_info())
        .plugin(Plugin::Attributes(Attributes { attribute_list: attributes_list }))
        .init_authority(PluginAuthority::UpdateAuthority)
        .invoke_signed(&[signer_seeds])?;
    }
    // If the Attributes Plugin exists, we update it
    else {
        UpdatePluginV1CpiBuilder::new(&ctx.accounts.mpl_core_program.to_account_info())
        .asset(&ctx.accounts.asset.to_account_info())
        .collection(Some(&ctx.accounts.collection.to_account_info()))
        .payer(&ctx.accounts.owner.to_account_info())
        .authority(Some(&ctx.accounts.update_authority.to_account_info()))
        .system_program(&ctx.accounts.system_program.to_account_info())
        .plugin(Plugin::Attributes(Attributes { attribute_list: attributes_list }))
        .invoke_signed(&[signer_seeds])?;
    }

    // --- Challenge 2: Increment Collection Stats ---
    
    // Fetch collection attributes
    let collection_attributes_fetched = fetch_plugin::<BaseCollectionV1, Attributes>(
        &ctx.accounts.collection.to_account_info(),
        PluginType::Attributes,
    ).ok().map(|(_, attrs, _)| attrs);

    let mut coll_attr_list: Vec<Attribute> = Vec::new();
    let mut total_staked: u32 = 0;
    let has_attributes = collection_attributes_fetched.is_some();

    if let Some(attrs) = collection_attributes_fetched {
        coll_attr_list = attrs.attribute_list;
        if let Some(attr) = coll_attr_list.iter().find(|a| a.key == "total_staked") {
            total_staked = attr.value.parse::<u32>().unwrap_or(0);
        }
    }

    total_staked = total_staked.checked_add(1).ok_or(ErrorCode::NumericalOverflow)?;
    update_attribute(&mut coll_attr_list, "total_staked", total_staked.to_string());

    // Update collection attributes
    if !has_attributes {
        AddPluginV1CpiBuilder::new(&ctx.accounts.mpl_core_program.to_account_info())
        .asset(&ctx.accounts.collection.to_account_info())
        .collection(None)
        .payer(&ctx.accounts.owner.to_account_info())
        .authority(Some(&ctx.accounts.update_authority.to_account_info()))
        .system_program(&ctx.accounts.system_program.to_account_info())
        .plugin(Plugin::Attributes(Attributes { attribute_list: coll_attr_list }))
        .init_authority(PluginAuthority::UpdateAuthority)
        .invoke_signed(&[signer_seeds])?;
    } else {
        UpdatePluginV1CpiBuilder::new(&ctx.accounts.mpl_core_program.to_account_info())
        .asset(&ctx.accounts.collection.to_account_info())
        .collection(None)
        .payer(&ctx.accounts.owner.to_account_info())
        .authority(Some(&ctx.accounts.update_authority.to_account_info()))
        .system_program(&ctx.accounts.system_program.to_account_info())
        .plugin(Plugin::Attributes(Attributes { attribute_list: coll_attr_list }))
        .invoke_signed(&[signer_seeds])?;
    }

    // Freeze the asset with the FreezeDelegate Plugin
    // Note that the FreezeDelegate is a Owner-Managed Plugin, so it needs to be signed by the owner
    AddPluginV1CpiBuilder::new(&ctx.accounts.mpl_core_program.to_account_info())
    .asset(&ctx.accounts.asset.to_account_info())
    .collection(Some(&ctx.accounts.collection.to_account_info()))
    .payer(&ctx.accounts.owner.to_account_info())
    .authority(Some(&ctx.accounts.owner.to_account_info()))
    .system_program(&ctx.accounts.system_program.to_account_info())
    .plugin(Plugin::FreezeDelegate(FreezeDelegate { frozen: true }))
    .init_authority(PluginAuthority::UpdateAuthority)
    .invoke()?;

    Ok(())
}