//! REST client for `savr-server` (PRD-04). Speaks only `savr_core::protocol`
//! types over HTTPS — the daemon never imports the server crate.
//!
//! Auth (PRD-06 §3): a short-lived access token is sent as `Bearer` on every
//! call; on `401` we exchange the device's refresh secret for a fresh token and
//! retry once. See the integration notes for how this maps onto the current M1
//! server (which accepts the raw account UUID as the bearer until JWT lands).

use std::path::Path;
use std::sync::Arc;

use futures_util::StreamExt;
use reqwest::StatusCode;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use savr_core::protocol::{
    EnsureGameRequest, HeadResponse, PairRequest, PairResponse, RefreshRequest, ResolveRequest,
    TokenResponse,
};
use savr_core::types::Os;
use savr_core::{Blake3Hash, CreateVersion, GameId, SyncedConfig, Version};

use crate::secrets::Credentials;

/// A game as the server returns it from `POST /games` (a lean row, not the full
/// `Game` with save targets — those live client-side).
#[derive(Debug, Clone, Deserialize)]
pub struct EnsuredGame {
    pub id: GameId,
    pub title: String,
    pub steam_appid: Option<u32>,
    pub head: Option<uuid::Uuid>,
}

/// Result of `create_version`: fast-forward accepted, or a divergent-branch
/// conflict carrying both tips (PRD-03 §4).
#[derive(Debug)]
pub enum CreateOutcome {
    Created(Version),
    Conflict {
        head: Option<Version>,
        incoming: Version,
    },
}

pub struct ServerClient {
    http: reqwest::Client,
    /// `<server_url>/api/v1`.
    base: String,
    /// Short-lived access token (PRD-06 §3), refreshed on demand.
    access: Arc<Mutex<Option<String>>>,
    /// Device credential for refresh; None until paired.
    creds: Arc<Mutex<Option<Credentials>>>,
}

impl ServerClient {
    pub fn new(server_url: &str) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent("savr-daemon")
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        Ok(Self {
            http,
            base: format!("{}/api/v1", server_url.trim_end_matches('/')),
            access: Arc::new(Mutex::new(None)),
            creds: Arc::new(Mutex::new(None)),
        })
    }

    /// Seed a known credential (loaded from the secret store at boot).
    pub async fn set_credentials(&self, creds: Credentials) {
        *self.creds.lock().await = Some(creds);
    }

    /// Set the bearer directly. Used by the M1 bridge (account UUID as token)
    /// and by tests.
    pub async fn set_access_token(&self, token: String) {
        *self.access.lock().await = Some(token);
    }

    pub async fn is_authenticated(&self) -> bool {
        self.access.lock().await.is_some()
    }

    /// The current bearer token, if any (for the WebSocket upgrade header).
    pub async fn access_token(&self) -> Option<String> {
        self.access.lock().await.clone()
    }

    // ---- auth ----

    /// Pair this device with a one-time code (PRD-06 §2). Stores the returned
    /// credential + access token in memory; the caller persists the credential
    /// to the secret store.
    pub async fn pair(
        &self,
        code: &str,
        device_name: &str,
        os: Os,
    ) -> anyhow::Result<PairResponse> {
        let body = PairRequest {
            code: code.to_string(),
            device_name: device_name.to_string(),
            os,
        };
        let resp = self
            .http
            .post(format!("{}/devices/pair", self.base))
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let paired: PairResponse = resp.json().await?;
        *self.access.lock().await = Some(paired.access_token.clone());
        *self.creds.lock().await = Some(Credentials {
            device_id: paired.device_id,
            account_id: paired.account_id,
            refresh_secret: paired.refresh_secret.clone(),
        });
        Ok(paired)
    }

    /// Exchange the refresh secret for a fresh access token (PRD-06 §3).
    pub async fn refresh(&self) -> anyhow::Result<()> {
        let creds = self
            .creds
            .lock()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("cannot refresh: device is not paired"))?;
        let body = RefreshRequest {
            device_id: creds.device_id,
            refresh_secret: creds.refresh_secret,
        };
        let resp = self
            .http
            .post(format!("{}/auth/refresh", self.base))
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let token: TokenResponse = resp.json().await?;
        *self.access.lock().await = Some(token.access_token);
        Ok(())
    }

    /// Send a request with the current bearer; on `401`, refresh once and retry.
    async fn send_authed<F>(&self, make: F) -> anyhow::Result<reqwest::Response>
    where
        F: Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    {
        let token = self.access.lock().await.clone();
        let mut req = make(&self.http);
        if let Some(t) = &token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await?;
        if resp.status() != StatusCode::UNAUTHORIZED {
            return Ok(resp);
        }
        // Token expired/invalid → try a single refresh + retry (PRD-06 §3).
        if self.refresh().await.is_ok() {
            let token = self.access.lock().await.clone();
            let mut req = make(&self.http);
            if let Some(t) = &token {
                req = req.bearer_auth(t);
            }
            return Ok(req.send().await?);
        }
        Ok(resp)
    }

    // ---- games & config ----

    pub async fn ensure_game(
        &self,
        title: &str,
        steam_appid: Option<u32>,
    ) -> anyhow::Result<EnsuredGame> {
        let body = EnsureGameRequest {
            title: title.to_string(),
            steam_appid,
        };
        let url = format!("{}/games", self.base);
        let resp = self
            .send_authed(|http| http.post(&url).json(&body))
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }

    pub async fn get_head(&self, game_id: GameId) -> anyhow::Result<HeadResponse> {
        let url = format!("{}/games/{}/head", self.base, game_id);
        let resp = self
            .send_authed(|http| http.get(&url))
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }

    pub async fn list_versions(&self, game_id: GameId) -> anyhow::Result<Vec<Version>> {
        let url = format!("{}/games/{}/versions", self.base, game_id);
        let resp = self
            .send_authed(|http| http.get(&url))
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }

    pub async fn create_version(
        &self,
        game_id: GameId,
        req: &CreateVersion,
    ) -> anyhow::Result<CreateOutcome> {
        let url = format!("{}/games/{}/versions", self.base, game_id);
        let resp = self.send_authed(|http| http.post(&url).json(req)).await?;
        match resp.status() {
            StatusCode::CREATED | StatusCode::OK => Ok(CreateOutcome::Created(resp.json().await?)),
            StatusCode::CONFLICT => {
                let v: serde_json::Value = resp.json().await?;
                let detail = &v["error"]["detail"];
                let head = serde_json::from_value(detail["head"].clone()).ok();
                let incoming = serde_json::from_value(detail["incoming"].clone())?;
                Ok(CreateOutcome::Conflict { head, incoming })
            }
            s => {
                let text = resp.text().await.unwrap_or_default();
                Err(anyhow::anyhow!("create_version failed ({s}): {text}"))
            }
        }
    }

    /// Resolve a conflict server-side (PRD-04 §2). Not present on the M1 server
    /// yet — see integration notes.
    pub async fn resolve_conflict(
        &self,
        game_id: GameId,
        req: &ResolveRequest,
    ) -> anyhow::Result<()> {
        let url = format!("{}/games/{}/resolve", self.base, game_id);
        self.send_authed(|http| http.post(&url).json(req))
            .await?
            .error_for_status()?;
        Ok(())
    }

    pub async fn get_config(&self) -> anyhow::Result<SyncedConfig> {
        let url = format!("{}/config", self.base);
        let resp = self
            .send_authed(|http| http.get(&url))
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }

    /// PUT the config and return the server's stored version. The response
    /// carries the NEW optimistic-concurrency `tag`, which the caller must adopt
    /// locally — otherwise the next edit sends a stale tag and is rejected 409.
    pub async fn put_config(&self, cfg: &SyncedConfig) -> anyhow::Result<SyncedConfig> {
        let url = format!("{}/config", self.base);
        let resp = self
            .send_authed(|http| http.put(&url).json(cfg))
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }

    // ---- blobs ----

    pub async fn blob_exists(&self, hash: &Blake3Hash) -> anyhow::Result<bool> {
        let url = format!("{}/blobs/{}", self.base, hash.to_hex());
        let resp = self.send_authed(|http| http.head(&url)).await?;
        Ok(resp.status().is_success())
    }

    /// Upload an archive file to the blob store.
    ///
    /// ponytail: reads the whole `.savr` into memory before `PUT`. Fine for the
    /// small archives saves produce; swap to a streamed body + `Content-Range`
    /// resume (PRD-04 §2) when large-save support lands.
    pub async fn blob_put_file(&self, hash: &Blake3Hash, path: &Path) -> anyhow::Result<()> {
        let bytes = tokio::fs::read(path).await?;
        let url = format!("{}/blobs/{}", self.base, hash.to_hex());
        self.send_authed(|http| http.put(&url).body(bytes.clone()))
            .await?
            .error_for_status()?;
        Ok(())
    }

    /// Download a blob to `dest`, streaming so a large archive never buffers
    /// whole (PRD-04 §6).
    pub async fn blob_get_to_file(&self, hash: &Blake3Hash, dest: &Path) -> anyhow::Result<()> {
        let url = format!("{}/blobs/{}", self.base, hash.to_hex());
        let resp = self
            .send_authed(|http| http.get(&url))
            .await?
            .error_for_status()?;
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut file = tokio::fs::File::create(dest).await?;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            file.write_all(&chunk?).await?;
        }
        file.flush().await?;
        Ok(())
    }
}
