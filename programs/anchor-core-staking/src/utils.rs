use anchor_lang::prelude::*;
use mpl_core::types::{Attribute, Attributes};
use crate::error::ErrorCode;

pub const SECONDS_PER_DAY: i64 = 86400;

pub fn calculate_rewards(
    staked_time_days: u64,
    rewards_bps: u16,
    decimals: u8,
) -> Result<u64> {
    staked_time_days
        .checked_mul(rewards_bps as u64)
        .ok_or(ErrorCode::InvalidRewardsBps)?
        .checked_mul(10u64.pow(decimals as u32))
        .ok_or(ErrorCode::InvalidRewardsBps)?
        .checked_div(10000u64)
        .ok_or(error!(ErrorCode::InvalidRewardsBps))
}

pub fn get_attribute_value(attributes: &Attributes, key: &str) -> Option<String> {
    attributes
        .attribute_list
        .iter()
        .find(|a| a.key == key)
        .map(|a| a.value.clone())
}

pub fn update_attribute(attributes: &mut Vec<Attribute>, key: &str, value: String) {
    if let Some(attr) = attributes.iter_mut().find(|a| a.key == key) {
        attr.value = value;
    } else {
        attributes.push(Attribute {
            key: key.to_string(),
            value,
        });
    }
}
