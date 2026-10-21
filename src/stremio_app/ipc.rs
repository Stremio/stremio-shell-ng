use serde::{Deserialize, Serialize};
use serde_json::{self, json};
use std::cell::RefCell;

use crate::stremio_app::gpu_video_processing;

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub type Channel = RefCell<Option<(flume::Sender<String>, flume::Receiver<String>)>>;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheDirectoryRequest {
    pub request_id: u64,
    pub server_url: String,
    pub directory: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RPCRequest {
    pub id: u64,
    pub args: Option<Vec<serde_json::Value>>,
}

impl RPCRequest {
    pub fn is_handshake(&self) -> bool {
        self.id == 0
    }
    pub fn get_method(&self) -> Option<&str> {
        self.args
            .as_ref()
            .and_then(|args| args.first())
            .and_then(|arg| arg.as_str())
    }
    pub fn get_params(&self) -> Option<&serde_json::Value> {
        self.args
            .as_ref()
            .and_then(|args| if args.len() > 1 { Some(&args[1]) } else { None })
    }
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
    pub fn get_handshake(streaming_server_url: Option<&str>) -> String {
        let resp = RPCResponse {
            id: 0,
            object: "transport".to_string(),
            response_type: 3,
            data: Some(RPCResponseData {
                transport: RPCResponseDataTransport {
                    properties: vec![
                        vec![],
                        vec![
                            "".to_string(),
                            "shellVersion".to_string(),
                            "".to_string(),
                            VERSION.to_string(),
                        ],
                        vec![
                            "".to_string(),
                            "gpuVideoProcessing".to_string(),
                            "".to_string(),
                            gpu_video_processing::gpu_video_processing_supported().to_string(),
                        ],
                        vec![
                            "".to_string(),
                            "cacheDirectoryPicker".to_string(),
                            "".to_string(),
                            streaming_server_url.is_some().to_string(),
                        ],
                        vec![
                            "".to_string(),
                            "streamingServerUrl".to_string(),
                            "".to_string(),
                            streaming_server_url.unwrap_or_default().to_string(),
                        ],
                        vec![
                            "".to_string(),
                            "nativeInterfaceScale".to_string(),
                            "".to_string(),
                            "true".to_string(),
                        ],
                        vec![
                            "".to_string(),
                            "nativeAssSubtitles".to_string(),
                            "".to_string(),
                            "true".to_string(),
                        ],
                    ],
                    signals: vec![],
                    methods: vec![vec!["onEvent".to_string(), "".to_string()]],
                },
            }),
            ..Default::default()
        };
        serde_json::to_string(&resp).expect("Cannot build response")
    }
    pub fn response_message(msg: Option<serde_json::Value>) -> String {
        let resp = RPCResponse {
            id: 1,
            object: "transport".to_string(),
            response_type: 1,
            args: msg,
            ..Default::default()
        };
        serde_json::to_string(&resp).expect("Cannot build response")
    }
    pub fn visibility_change(visible: bool, visibility: u32, is_full_screen: bool) -> String {
        Self::response_message(Some(json!(["win-visibility-changed" ,{
            "visible": visible,
            "visibility": visibility,
            "isFullscreen": is_full_screen
        }])))
    }
    pub fn pip_change(pip: bool) -> String {
        Self::response_message(Some(json!(["win-pip-changed", { "pip": pip }])))
    }
    pub fn state_change(state: u32) -> String {
        Self::response_message(Some(json!(["win-state-changed" ,{
            "state": state,
        }])))
    }
    pub fn update_available() -> String {
        Self::response_message(Some(json!(["autoupdater-show-notif"])))
    }
    pub fn media_key(action: &str) -> String {
        Self::response_message(Some(json!(["media-key", action])))
    }
    /// Réponse du pont HTTP natif (voir native_http.rs) : `native-http-response` avec l'identifiant de la requête.
    pub fn native_http_response(payload: serde_json::Value) -> String {
        Self::response_message(Some(json!(["native-http-response", payload])))
    }
    /// Morceau du pont de lecture par plages (voir native_range.rs), émis pendant la lecture.
    pub fn native_range_chunk(payload: serde_json::Value) -> String {
        Self::response_message(Some(json!(["native-range-chunk", payload])))
    }
    /// Réponse finale du pont de lecture par plages (voir native_range.rs) : statut et en-têtes, ou erreur.
    pub fn native_range_response(payload: serde_json::Value) -> String {
        Self::response_message(Some(json!(["native-range-response", payload])))
    }
    /// Capacités du pont de lecture par plages, en réponse à `native-range-caps`.
    pub fn native_range_caps(payload: serde_json::Value) -> String {
        Self::response_message(Some(json!(["native-range-caps", payload])))
    }
    /// Réponse d'une méthode `download-*` (voir downloads.rs) : `["nom-de-méthode", {ok, ...}]`.
    pub fn download_reply(method: &str, payload: serde_json::Value) -> String {
        Self::response_message(Some(json!([method, payload])))
    }
    pub fn cache_directory_selected(
        request_id: u64,
        result: Result<Option<String>, String>,
    ) -> String {
        let (path, error) = match result {
            Ok(path) => (path, None),
            Err(error) => (None, Some(error)),
        };
        Self::response_message(Some(json!(["cache-directory-selected", {
            "requestId": request_id,
            "path": path,
            "error": error,
        }])))
    }
}
