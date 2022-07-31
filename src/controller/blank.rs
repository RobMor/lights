use crate::color::{Color, NUM_LIGHTS, OFF};
use crate::controller::Controller;

pub struct BlankController;

impl BlankController {
    pub fn new() -> Self {
        Self
    }
}

impl Controller for BlankController {
    fn is_active(&self) -> bool {
        true
    }

    fn tick(&mut self) -> [Color; NUM_LIGHTS] {
        OFF
    }
}
