pub mod app;
pub mod command;
pub mod component;
pub mod components;
pub mod keybindings;
pub mod layout;
pub mod markdown;
pub mod message;
pub mod overlay;
pub mod theme;

// Legacy monolithic TUI — will be removed once all tasks are complete.
mod legacy;
pub use legacy::run_tui;
