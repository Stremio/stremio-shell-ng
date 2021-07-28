use serde::{Deserialize, Serialize};

const JSON_RESPONSES: [&str; 3] = ["track-list", "video-params", "metadata"];

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PlayerProprChange {
    name: String,
    data: serde_json::Value,
}
impl PlayerProprChange {
    fn value_from_format(data: mpv::Format, as_json: bool) -> serde_json::Value {
        match data {
            mpv::Format::Flag(d) => serde_json::Value::Bool(d),
            mpv::Format::Int(d) => serde_json::Value::Number(
                serde_json::Number::from_f64(d as f64).expect("MPV returned invalid number"),
            ),
            mpv::Format::Double(d) => serde_json::Value::Number(
                serde_json::Number::from_f64(d).expect("MPV returned invalid number"),
            ),
            mpv::Format::OsdStr(s) => serde_json::Value::String(s.to_string()),
            mpv::Format::Str(s) => {
                if as_json {
                    serde_json::from_str(s).expect("MPV returned invalid JSON data")
                } else {
                    serde_json::Value::String(s.to_string())
                }
            }
        }
    }
    pub fn from_name_value(name: String, value: mpv::Format) -> Self {
        let is_json = JSON_RESPONSES.contains(&name.as_str());
        Self {
            name,
            data: Self::value_from_format(value, is_json),
        }
    }
}
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PlayerEnded {
    reason: String,
}
impl PlayerEnded {
    fn string_from_end_reason(data: mpv::EndFileReason) -> String {
        match data {
            mpv::EndFileReason::MPV_END_FILE_REASON_ERROR => "error".to_string(),
            mpv::EndFileReason::MPV_END_FILE_REASON_QUIT => "quit".to_string(),
            _ => "other".to_string(),
        }
    }
    pub fn from_end_reason(data: mpv::EndFileReason) -> Self {
        Self {
            reason: Self::string_from_end_reason(data),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PlayerError {
    pub error: String,
}
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum PlayerEvent {
    PropChange(PlayerProprChange),
    End(PlayerEnded),
    Error(PlayerError),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PlayerResponse<'a>(pub &'a str, pub PlayerEvent);
impl PlayerResponse<'_> {
    pub fn to_value(&self) -> Option<serde_json::Value> {
        serde_json::to_value(self).ok()
    }
}
