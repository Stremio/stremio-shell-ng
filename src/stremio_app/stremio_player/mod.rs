pub mod player;
pub use player::Player;
pub mod communication;
pub use communication::{PlayerEnded, PlayerError, PlayerEvent, PlayerProprChange, PlayerResponse};
