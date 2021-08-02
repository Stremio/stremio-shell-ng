pub mod player;
pub use player::Player;
pub mod communication;
pub use communication::{
    BoolProp, CmdVal, FpProp, InMsg, InMsgArgs, InMsgFn, IntProp, MpvCmd, PlayerEnded, PlayerError,
    PlayerEvent, PlayerProprChange, PlayerResponse, PropKey, PropVal, StrProp,
};
#[cfg(test)]
mod communication_tests;
