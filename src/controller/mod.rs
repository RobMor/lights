use crate::color::{Color, NUM_LIGHTS};

pub mod blank;
pub mod music;

pub trait Controller {
    fn is_active(&self) -> bool;
    fn tick(&mut self) -> [Color; NUM_LIGHTS];
}