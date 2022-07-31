mod magma;
mod inferno;

// TODO scarlet seems to be causing us to be linked against libc
pub use magma::MAGMA_DATA;
pub use inferno::INFERNO_DATA;