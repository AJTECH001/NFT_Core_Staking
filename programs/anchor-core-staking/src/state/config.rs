use anchor_lang::prelude::*;

#[account]
#[derive(InitSpace)]
pub struct Config {
    pub rewards_bps: u16,       // rewards percentage in basis points
    pub freeze_period: u16,     // minimum freeze period in days
    pub rewards_bump: u8,       // Bumps for the rewards mint account
    pub bump: u8,               // Bumps for the config account
}