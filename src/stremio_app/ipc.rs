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
/// Forme attendue du paramètre (`args[1]`) d'une commande de la liste blanche.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    /// Aucun paramètre lu.
    Unused,
    /// Le gestionnaire valide son argument et répond lui-même en cas d'erreur
    /// (`download-*`, `native-range`) : la liste ne doit pas les faire taire.
    SelfChecked,
    /// Objet JSON requis.
    Object,
    /// Chaîne JSON requise.
    Str,
    /// Booléen JSON requis.
    Bool,
    /// Tableau JSON requis.
    Array,
}

/// Commandes que la page peut envoyer au shell, avec la forme attendue de leur paramètre.
///
/// Seule source de vérité : une méthode absente de cette liste est refusée et journalisée
/// avant tout traitement (`app.rs`). Une méthode présente doit avoir son traitement dans
/// `app.rs`. Ajouter une commande = la déclarer ici avec sa forme, puis la traiter.
/// Les `download-*` (`downloads::is_worker_method`) et `mpv-*` (`stremio_player`) sont listés un à un.
pub const ALLOWED_COMMANDS: &[(&str, ParamKind)] = &[
    // Fenêtre et interface
    ("win-set-interface-scale", ParamKind::Object),
    ("win-set-visibility", ParamKind::Object),
    ("win-set-pip", ParamKind::Object),
    ("win-drag", ParamKind::Unused),
    ("win-focus", ParamKind::Unused),
    // Cycle de vie
    ("app-ready", ParamKind::Unused),
    ("app-error", ParamKind::Unused),
    ("quit", ParamKind::Unused),
    ("autoupdater-notif-clicked", ParamKind::Unused),
    // Liens et lecteur externes
    ("open-external", ParamKind::Str),
    ("play-external", ParamKind::Str),
    // Sélecteur de dossier du cache
    ("pick-cache-directory", ParamKind::Object),
    // Téléchargements
    ("download-folder-pick", ParamKind::Unused),
    ("download-folder-get", ParamKind::Unused),
    ("download-usage", ParamKind::Unused),
    ("download-open", ParamKind::SelfChecked),
    ("download-write", ParamKind::SelfChecked),
    ("download-truncate", ParamKind::SelfChecked),
    ("download-size", ParamKind::SelfChecked),
    ("download-finalize", ParamKind::SelfChecked),
    ("download-path", ParamKind::SelfChecked),
    ("download-remove", ParamKind::SelfChecked),
    // Ponts HTTP et lecture par plages
    ("native-http", ParamKind::Object),
    ("native-addon-http", ParamKind::Object),
    ("native-box-playlist", ParamKind::Object),
    ("native-range", ParamKind::SelfChecked),
    ("native-range-cancel", ParamKind::Object),
    ("native-range-caps", ParamKind::Unused),
    // Lecteur mpv
    ("mpv-set-prop", ParamKind::Array),
    ("mpv-command", ParamKind::Array),
    ("mpv-observe-prop", ParamKind::Str),
    ("mpv-set-gpu-video-processing", ParamKind::Bool),
];

/// Vérifie qu'une méthode est dans la liste blanche et que son paramètre a la forme attendue.
/// Renvoie la raison du refus, sans la valeur du paramètre (rien de sensible dans le journal).
pub fn check_command(method: &str, params: Option<&serde_json::Value>) -> Result<(), &'static str> {
    let Some((_, kind)) = ALLOWED_COMMANDS.iter().find(|(name, _)| *name == method) else {
        return Err("command not in the allowlist");
    };
    let shape_ok = match kind {
        ParamKind::Unused | ParamKind::SelfChecked => true,
        ParamKind::Object => params.is_some_and(|p| p.is_object()),
        ParamKind::Str => params.is_some_and(|p| p.is_string()),
        ParamKind::Bool => params.is_some_and(|p| p.is_boolean()),
        ParamKind::Array => params.is_some_and(|p| p.is_array()),
    };
    if shape_ok {
        Ok(())
    } else {
        Err("argument has the wrong JSON type")
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
                        vec![
                            "".to_string(),
                            "downloadStore".to_string(),
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
        Self::serialize(&resp)
    }
    /// Sérialise une réponse ; en cas d'échec (jamais attendu) l'erreur est journalisée et un objet vide part à la place.
    fn serialize(resp: &RPCResponse) -> String {
        serde_json::to_string(resp).unwrap_or_else(|error| {
            crate::stremio_app::robust::report_error("ipc: réponse non sérialisable", &error);
            "{}".to_owned()
        })
    }
    pub fn response_message(msg: Option<serde_json::Value>) -> String {
        let resp = RPCResponse {
            id: 1,
            object: "transport".to_string(),
            response_type: 1,
            args: msg,
            ..Default::default()
        };
        Self::serialize(&resp)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_params(kind: ParamKind) -> Option<serde_json::Value> {
        match kind {
            ParamKind::Unused | ParamKind::SelfChecked => None,
            ParamKind::Object => Some(json!({})),
            ParamKind::Str => Some(json!("https://example.com/")),
            ParamKind::Bool => Some(json!(true)),
            ParamKind::Array => Some(json!([])),
        }
    }

    #[test]
    fn listed_commands_are_accepted_with_their_parameter_shape() {
        for (name, kind) in ALLOWED_COMMANDS {
            let params = sample_params(*kind);
            assert!(
                check_command(name, params.as_ref()).is_ok(),
                "{} must be accepted with a {:?} parameter",
                name,
                kind
            );
        }
    }

    #[test]
    fn unlisted_commands_are_refused() {
        for method in [
            "eval-script",
            "download-delete",
            "mpv-unknown",
            "QUIT",
            "quit ",
            "",
        ] {
            assert!(
                check_command(method, None).is_err(),
                "{:?} is not in the allowlist and must be refused",
                method
            );
        }
    }

    #[test]
    fn a_listed_command_with_the_wrong_parameter_type_is_refused() {
        assert!(check_command("open-external", Some(&json!(42))).is_err());
        assert!(check_command("win-set-pip", None).is_err());
        assert!(check_command("mpv-command", Some(&json!("stop"))).is_err());
        assert!(check_command("mpv-set-gpu-video-processing", Some(&json!("yes"))).is_err());
    }

    #[test]
    fn the_allowlist_has_no_duplicate_names() {
        let mut names: Vec<&str> = ALLOWED_COMMANDS.iter().map(|(name, _)| *name).collect();
        let listed = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), listed);
    }

    #[test]
    fn handshake_announces_the_download_store() {
        let wire: serde_json::Value =
            serde_json::from_str(&RPCResponse::get_handshake(None)).unwrap();
        let properties = wire["data"]["transport"]["properties"].as_array().unwrap();
        let announced = properties
            .iter()
            .find(|entry| entry[1] == "downloadStore")
            .map(|entry| entry[3].clone());
        assert_eq!(announced, Some(json!("true")));
    }
}
