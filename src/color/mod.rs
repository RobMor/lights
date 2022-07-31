pub mod cmap;

pub const NUM_LIGHTS: usize = 3;

#[derive(Debug, Copy, Clone)]
pub struct Color {
    pub i: u8,
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

pub const OFF: [Color; NUM_LIGHTS] = [
    Color { i: 0, r: 0, g: 0, b: 0},
    Color { i: 0, r: 0, g: 0, b: 0},
    Color { i: 0, r: 0, g: 0, b: 0},
];