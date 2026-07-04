//! Wire contracts beyond the core entities: REST request/response DTOs
//! (PRD-04 §2, PRD-06 §2–3) and the WebSocket push protocol (PRD-04 §4).
//! Server and daemon both compile this module — the messages cannot drift.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{AccountId, DeviceId, GameId, Os, VersionId};

// ---- REST: auth & pairing (PRD-06) ----

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LoginRequest {
    pub password: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LoginResponse {
    /// Owner session token, used to mint pairing codes.
    pub session_token: String,
    pub expires_in: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PairingCodeResponse {
    pub code: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PairRequest {
    pub code: String,
    pub device_name: String,
    pub os: Os,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PairResponse {
    pub device_id: DeviceId,
    pub account_id: AccountId,
    /// Long-lived refresh secret — returned exactly once, stored in the OS
    /// keychain client-side, hashed at rest server-side (PRD-06 §3).
    pub refresh_secret: String,
    pub access_token: String,
    pub expires_in: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RefreshRequest {
    pub device_id: DeviceId,
    pub refresh_secret: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TokenResponse {
    pub access_token: String,
    pub expires_in: u64,
}

// ---- REST: games & versions ----

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EnsureGameRequest {
    pub title: String,
    pub steam_appid: Option<u32>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HeadResponse {
    pub head: Option<VersionId>,
    pub seq: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ResolveRequest {
    /// The version that becomes the new head.
    pub winner: VersionId,
    /// Keep the loser's files restorable under a sibling folder (PRD-03 §4).
    #[serde(default)]
    pub keep_both: bool,
}

// ---- WebSocket push (PRD-04 §4) ----

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    VersionAvailable {
        game_id: GameId,
        version_id: VersionId,
        seq: u64,
    },
    Conflict {
        game_id: GameId,
        tips: Vec<VersionId>,
    },
    ConfigUpdated {
        config_tag: String,
    },
    DeviceAdded {
        device_id: DeviceId,
    },
    Pong,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// On connect: identify + report the last seq seen per game so the server
    /// can replay what was missed while offline (PRD-03 §5).
    Hello {
        device_id: DeviceId,
        #[serde(default)]
        last_seq: HashMap<GameId, u64>,
    },
    Subscribe {
        /// Game ids, or `["*"]` for everything on the account.
        games: Vec<String>,
    },
    Ping,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_messages_use_snake_case_type_tags() {
        let msg = ServerMsg::VersionAvailable {
            game_id: uuid::Uuid::nil(),
            version_id: uuid::Uuid::nil(),
            seq: 42,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "version_available");
        assert_eq!(json["seq"], 42);

        let hello: ClientMsg = serde_json::from_str(
            r#"{"type":"hello","device_id":"00000000-0000-0000-0000-000000000000"}"#,
        )
        .unwrap();
        assert!(matches!(hello, ClientMsg::Hello { .. }));
    }
}
