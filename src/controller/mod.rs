use tokio::sync::mpsc;

pub mod blank;
pub mod music;

use crate::NUM_LIGHTS;
use crate::Color;

#[derive(Debug)]
pub enum InMessage {
    GrantAccess(mpsc::Sender<[Color; NUM_LIGHTS]>),
    RevokeAccess,
}

#[derive(Debug)]
pub enum OutMessage {
    RequestAccess,
    RescindAccess(mpsc::Sender<[Color; NUM_LIGHTS]>),
}

#[derive(Hash, Eq, PartialEq, Ord, PartialOrd, Copy, Clone, Debug)]
pub struct Token {
    priority: u8,
}

impl Token {
    pub fn new(unique_priority: u8) -> Token {
        Token {
            priority: unique_priority,
        }
    }
}
