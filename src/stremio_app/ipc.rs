use std::cell::RefCell;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use serde::{Deserialize, Serialize};
use serde_json::{self, json};

pub type Channel = RefCell<Option<(mpsc::Sender<String>, Arc<Mutex<mpsc::Receiver<String>>>)>>;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RPCRequest {
    pub id: u64,
    pub args: Option<Vec<serde_json::Value>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RPCResponseDataTransport {
    pub properties: Vec<Vec<String>>,
    pub signals: Vec<String>,
    pub methods: Vec<Vec<String>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RPCResponseData {
    pub transport: RPCResponseDataTransport,
}

#[derive(Default, Serialize, Deserialize, Debug, Clone)]
pub struct RPCResponse {
    pub id: u64,
    pub object: String,
    #[serde(rename = "type")]
    pub response_type: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<RPCResponseData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
}

impl RPCResponse {
    pub fn visibility_change(visible: bool, visibility: u32, is_full_screen: bool) -> String {
        let resp = RPCResponse {
            id: 1,
            object: "transport".to_string(),
            response_type: 1,
            args: Some(json!(["win-visibility-changed" ,{
                "visible": visible,
                "visibility": visibility,
                "isFullscreen": is_full_screen
            }])),
            ..Default::default()
        };
        serde_json::to_string(&resp).expect("Cannot build response")
    }
}
