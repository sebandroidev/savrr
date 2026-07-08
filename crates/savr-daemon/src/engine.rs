//! The engine: the daemon's brain. Owns local state, the server client, the
//! resolved games catalog, and the exe index; dispatches every `GuiRequest`
//! (PRD-05 §4) and drives event-triggered backups (PRD-03 §2).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{broadcast, RwLock};

use savr_core::ipc::{
    DaemonMsg, DaemonStatus, DetectionEvent, GuiRequest, ResolveChoice, RootKind, RootSpec,
};
use savr_core::manifest::{Manifest, Roots};
use savr_core::protocol::ResolveRequest;
use savr_core::types::Os;
use savr_core::{DeviceId, Game, GameId, GameSource, SyncedConfig, VersionId};

use crate::backup::{run_backup, BackupJob, BackupOutcome};
use crate::client::ServerClient;
use crate::config::DaemonConfig;
use crate::detection::steam::{self, SteamLibrary};
use crate::detection::{ExeIndex, SharedExeIndex};
use crate::paths::{resolve_custom, resolve_game, ResolveContext, ResolvedGame};
use crate::restore::{run_restore, RestoreRequest};
use crate::secrets::SecretStore;
use crate::state::LocalState;

/// A game known to the daemon, with its resolved save locations.
#[derive(Clone)]
pub struct GameEntry {
    pub game: Game,
    pub resolved: ResolvedGame,
}

impl GameEntry {
    fn backup_job(&self) -> BackupJob {
        BackupJob {
            game_id: self.game.id,
            patterns: self.resolved.patterns.clone(),
            anchor: self.resolved.anchor.clone(),
            registry_keys: self.resolved.registry_keys.clone(),
            excludes: self.resolved.excludes.clone(),
        }
    }
}

/// The two divergent tips of an unresolved conflict (PRD-03 §4).
#[derive(Clone, Copy)]
struct ConflictTips {
    mine: VersionId,
    theirs: VersionId,
}

pub struct Engine {
    pub config: DaemonConfig,
    pub state: LocalState,
    client: Arc<ServerClient>,
    secret_store: Arc<dyn SecretStore>,
    events: broadcast::Sender<DetectionEvent>,
    exe_index: SharedExeIndex,
    manifest: RwLock<Manifest>,
    games: RwLock<HashMap<GameId, GameEntry>>,
    conflicts: RwLock<HashMap<GameId, ConflictTips>>,
    running: RwLock<HashSet<GameId>>,
    learn_mode: RwLock<Option<GameId>>,
    server_connected: Arc<AtomicBool>,
    device_id: RwLock<Option<DeviceId>>,
    started_at: Instant,
    blob_cache: PathBuf,
    scratch: PathBuf,
}

impl Engine {
    pub async fn new(
        config: DaemonConfig,
        state: LocalState,
        secret_store: Arc<dyn SecretStore>,
        events: broadcast::Sender<DetectionEvent>,
    ) -> anyhow::Result<Arc<Self>> {
        let client = Arc::new(ServerClient::new(&config.server_url)?);
        let mut device_id = None;

        // Seed the client from a stored credential (PRD-06 §3).
        if let Some(creds) = secret_store.load()? {
            device_id = Some(creds.device_id);
            client.set_credentials(creds.clone()).await;
            // M1 bridge: the current server accepts the raw account UUID as the
            // bearer (see integration notes). Once JWT lands, `refresh()`
            // replaces this on the first 401.
            client.set_access_token(creds.account_id.to_string()).await;
        }

        let data = crate::config::data_root();
        let engine = Arc::new(Self {
            config,
            state,
            client,
            secret_store,
            events,
            exe_index: Arc::new(RwLock::new(ExeIndex::new())),
            manifest: RwLock::new(Manifest::new()),
            games: RwLock::new(HashMap::new()),
            conflicts: RwLock::new(HashMap::new()),
            running: RwLock::new(HashSet::new()),
            learn_mode: RwLock::new(None),
            server_connected: Arc::new(AtomicBool::new(false)),
            device_id: RwLock::new(device_id),
            started_at: Instant::now(),
            blob_cache: data.join("blob_cache"),
            scratch: data.join("scratch"),
        });
        Ok(engine)
    }

    pub fn events(&self) -> broadcast::Sender<DetectionEvent> {
        self.events.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DetectionEvent> {
        self.events.subscribe()
    }

    pub fn exe_index(&self) -> SharedExeIndex {
        self.exe_index.clone()
    }

    pub fn client(&self) -> Arc<ServerClient> {
        self.client.clone()
    }

    pub fn server_connected_flag(&self) -> Arc<AtomicBool> {
        self.server_connected.clone()
    }

    pub async fn device_id(&self) -> Option<DeviceId> {
        *self.device_id.read().await
    }

    /// Swap in a freshly-fetched manifest (startup + daily refresh).
    pub async fn set_manifest(&self, manifest: Manifest) {
        *self.manifest.write().await = manifest;
    }

    /// Rebuild the games catalog + exe index from registered Steam roots and
    /// the manifest (PRD-02 §1, §3.2). Idempotent; safe to call on any change.
    ///
    /// `enrich_ids`: when true and paired, resolve each game's canonical server
    /// id via `ensure_game` (a serial HTTP call per game). Pass false for the
    /// fast pre-manifest startup pass — it lists games from local state only, so
    /// the GUI fills instantly without N network round-trips, and no game is
    /// registered server-side under its raw Steam folder name before the
    /// manifest supplies the canonical title.
    pub async fn refresh_games(&self, enrich_ids: bool) -> anyhow::Result<()> {
        let steam_libs = self.discover_steam_libraries().await?;
        let manifest = self.manifest.read().await;

        // Index manifest entries by their Steam appid for quick matching.
        let mut by_appid: HashMap<u32, (&String, savr_core::manifest::ManifestEntry)> =
            HashMap::new();
        for (title, entry) in manifest.iter() {
            if let Some(steam) = entry.steam {
                by_appid.insert(steam.id, (title, entry.clone()));
            }
        }

        let roots = Roots::current();
        let ctx = ResolveContext {
            roots: &roots,
            steam_libs: &steam_libs,
        };
        let overrides = self
            .state
            .synced_config()
            .await?
            .map(|c| c.overrides)
            .unwrap_or_default();
        let authed = enrich_ids && self.client.is_authenticated().await;

        let mut games = HashMap::new();
        let mut index = ExeIndex::new();

        for lib in &steam_libs {
            for sg in &lib.games {
                // A manifest match enriches the game with a real title and known
                // save paths. Without one we still list the game from its Steam
                // install and detect it by its executables; its save paths get
                // learned on first play instead of being a reason to hide it.
                let manifest_match = by_appid.get(&sg.appid);
                let title = manifest_match
                    .map(|(t, _)| (*t).clone())
                    .unwrap_or_else(|| sg.name.clone());

                // Respect a per-game ignore.
                if self
                    .config
                    .games
                    .get(title.as_str())
                    .map(|g| g.ignore)
                    .unwrap_or(false)
                {
                    continue;
                }

                let game_id = self.game_id_for(Some(sg.appid), &title, authed).await;
                let (source, save_targets) = match manifest_match {
                    Some((_, entry)) => (GameSource::Manifest, entry.save_targets()),
                    None => (GameSource::Steam, Vec::new()),
                };
                let game = Game {
                    id: game_id,
                    title,
                    source,
                    steam_appid: Some(sg.appid),
                    save_targets,
                    // Overlaid fresh from play_stats + the running set in ListGames.
                    running: false,
                    last_played: None,
                    last_session_secs: None,
                    total_secs: 0,
                };
                let resolved = resolve_game(&game, &overrides, &ctx, None);
                index.index_install_dir(&lib.install_path(sg), game_id);
                games.insert(game_id, GameEntry { game, resolved });
            }
        }

        // Track normalized titles already in the catalog so a later source never
        // duplicates a game an earlier (higher-precedence) source already added.
        let mut seen: HashSet<String> = games
            .values()
            .map(|e| crate::naming::normalize_title(&e.game.title))
            .collect();

        // Capability A: auto-detect manifest-known games under generic "game
        // folder" (Drive) roots by matching each install-folder name to the
        // manifest.
        let matcher = crate::scan::ManifestMatcher::build(&manifest);
        for root in self.state.list_roots().await.unwrap_or_default() {
            if root.kind != RootKind::Drive {
                continue;
            }
            for (folder_name, install_dir) in
                crate::scan::scan_folder_root(std::path::Path::new(&root.path))
            {
                let Some(title) = matcher.match_folder(&folder_name) else {
                    continue;
                };
                let norm = crate::naming::normalize_title(&title);
                if seen.contains(&norm) {
                    continue;
                }
                let Some(entry) = manifest.get(&title) else {
                    continue;
                };
                if self
                    .config
                    .games
                    .get(title.as_str())
                    .map(|g| g.ignore)
                    .unwrap_or(false)
                {
                    continue;
                }
                seen.insert(norm);
                let game_id = self.game_id_for(None, &title, authed).await;
                let game = Game {
                    id: game_id,
                    title: title.clone(),
                    source: GameSource::Manifest,
                    steam_appid: None,
                    save_targets: entry.save_targets(),
                    running: false,
                    last_played: None,
                    last_session_secs: None,
                    total_secs: 0,
                };
                let resolved = resolve_game(&game, &overrides, &ctx, Some(&install_dir));
                index.index_install_dir(&install_dir, game_id);
                games.insert(game_id, GameEntry { game, resolved });
            }
        }

        // Capability B: hand-registered custom games (persisted).
        for cg in self.state.list_custom_games().await.unwrap_or_default() {
            let norm = crate::naming::normalize_title(&cg.title);
            if !seen.insert(norm) {
                continue;
            }
            let game_id = self.game_id_for(None, &cg.title, authed).await;
            let game = Game {
                id: game_id,
                title: cg.title.clone(),
                source: GameSource::Custom,
                steam_appid: None,
                save_targets: Vec::new(),
                running: false,
                last_played: None,
                last_session_secs: None,
                total_secs: 0,
            };
            let resolved = resolve_custom(&cg.save_root, &cg.include, &cg.exclude);
            if let Some(p) = &cg.install_path {
                let path = std::path::Path::new(p);
                if path.is_dir() {
                    index.index_install_dir(path, game_id);
                } else {
                    // An exe install_path: index the exe itself, plus its
                    // parent dir so sibling launcher exes are caught too
                    // (design spec: exe -> that exe + its parent dir).
                    index.insert_exe(path, game_id);
                    if let Some(parent) = path.parent() {
                        if parent.is_dir() {
                            index.index_install_dir(parent, game_id);
                        }
                    }
                }
            }
            games.insert(game_id, GameEntry { game, resolved });
        }

        let n = games.len();
        *self.games.write().await = games;
        // Persist + hot-swap the exe index (PRD-05 §3).
        self.state.replace_exe_index(&index.to_rows()).await?;
        *self.exe_index.write().await = index;
        tracing::info!("catalog refreshed: {n} watched games");
        // Tell the GUI to reload — it may have queried an empty catalog before
        // this first build finished (the catalog is populated after startup's
        // manifest fetch).
        let _ = self.events.send(DetectionEvent::CatalogUpdated);
        Ok(())
    }

    async fn discover_steam_libraries(&self) -> anyhow::Result<Vec<SteamLibrary>> {
        let mut libs = Vec::new();
        for root in self.state.list_roots().await? {
            if root.kind == RootKind::Steam {
                libs.extend(steam::discover_libraries(std::path::Path::new(&root.path)));
            }
        }
        Ok(libs)
    }

    /// Stable game id for a game, keyed by Steam appid or, for non-Steam
    /// games, by normalized name. When paired, prefer the server's
    /// canonical id so multiple devices agree on one id per game — but NEVER let
    /// a server hiccup (unreachable, token not ready at startup) fail the whole
    /// catalog refresh. On any failure, fall back to a cached or freshly-minted
    /// local id so the games always list. Infallible by design.
    ///
    /// ponytail: a game first seen offline keeps a local id; reconciling an
    /// already-uploaded local id with the server id is a follow-up.
    async fn game_id_for(&self, appid: Option<u32>, title: &str, authed: bool) -> GameId {
        let key = match appid {
            Some(a) => format!("gameid:steam:{a}"),
            None => format!("gameid:name:{}", crate::naming::normalize_title(title)),
        };
        // Distinguish a read ERROR from a genuinely-absent key: on a transient DB
        // hiccup we must NOT mint-and-persist a fresh id, or we'd clobber the
        // stable id still on disk and orphan all history keyed by it.
        let read = self.state.get_meta(&key).await;
        let read_ok = read.is_ok();
        let cached = read
            .ok()
            .flatten()
            .and_then(|s| uuid::Uuid::parse_str(&s).ok());

        if authed {
            match self.client.ensure_game(title, appid).await {
                Ok(ensured) => {
                    if cached != Some(ensured.id) {
                        let _ = self.state.set_meta(&key, &ensured.id.to_string()).await;
                    }
                    return ensured.id;
                }
                Err(e) => {
                    tracing::warn!("ensure_game failed for {title} ({e}); using a local id for now")
                }
            }
        }
        if let Some(id) = cached {
            return id;
        }
        let id = uuid::Uuid::now_v7();
        if read_ok {
            // Key genuinely absent — safe to persist this as the stable id.
            let _ = self.state.set_meta(&key, &id.to_string()).await;
        } else {
            tracing::warn!(
                "get_meta({key}) failed; using an ephemeral id this refresh, not persisting"
            );
        }
        id
    }

    // ---- IPC request dispatch (PRD-05 §4) ----

    pub async fn handle_request(&self, req: GuiRequest) -> DaemonMsg {
        match req {
            GuiRequest::ListGames => {
                let games = self.games.read().await;
                // Read the running set BEFORE the DB stats: on_event persists the
                // play row before setting the running flag, so this order means a
                // game seen running always has its stats row loaded here too.
                let running = self.running.read().await;
                let stats = self.state.play_stats().await.unwrap_or_default();
                let listing = games
                    .values()
                    .map(|e| {
                        let mut g = e.game.clone();
                        g.running = running.contains(&g.id);
                        if let Some(s) = stats.get(&g.id) {
                            g.last_played = s.last_played;
                            g.last_session_secs = s.last_session_secs;
                            g.total_secs = s.total_secs;
                        }
                        g
                    })
                    .collect();
                DaemonMsg::Games(listing)
            }
            GuiRequest::ListRoots => match self.state.list_roots().await {
                Ok(roots) => DaemonMsg::Roots(roots),
                Err(e) => err(e),
            },
            GuiRequest::AddRoot(spec) => self.add_root(spec).await,
            GuiRequest::RemoveRoot { id } => match self.state.remove_root(id).await {
                Ok(()) => {
                    let _ = self.refresh_games(true).await;
                    DaemonMsg::Ok
                }
                Err(e) => err(e),
            },
            GuiRequest::BackupNow { game_id } => self.backup_now(game_id).await,
            GuiRequest::ListVersions { game_id } => {
                match self.client.list_versions(game_id).await {
                    Ok(versions) => DaemonMsg::Versions(versions),
                    Err(e) => err(e),
                }
            }
            GuiRequest::Restore {
                game_id,
                version_id,
            } => self.restore(game_id, version_id).await,
            GuiRequest::ResolveConflict { game_id, choice } => {
                self.resolve_conflict(game_id, choice).await
            }
            GuiRequest::GetStatus => self.status().await,
            GuiRequest::GetConfig => {
                let cfg = self
                    .state
                    .synced_config()
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                DaemonMsg::Config(Box::new(cfg))
            }
            GuiRequest::UpdateConfig(cfg) => self.update_config(*cfg).await,
            GuiRequest::EnterLearnMode { game_id } => {
                *self.learn_mode.write().await = Some(game_id);
                // ponytail: learn-mode currently just records intent; capturing
                // the exe on next launch (PRD-02 §3.3) is a follow-up.
                DaemonMsg::Ok
            }
            GuiRequest::PairDevice {
                server_url,
                code,
                device_name,
            } => self.pair(&server_url, &code, &device_name).await,
            GuiRequest::SetAutostart { enabled } => match crate::autostart::set(enabled) {
                Ok(()) => DaemonMsg::Ok,
                Err(e) => err(e),
            },
            GuiRequest::AddCustomGame { spec } => {
                let cg = crate::state::CustomGame {
                    title: spec.title,
                    install_path: spec.install_path,
                    save_root: spec.save_root,
                    include: spec.include,
                    exclude: spec.exclude,
                };
                match self.state.add_custom_game(&cg).await {
                    Ok(()) => {
                        if let Err(e) = self.refresh_games(true).await {
                            tracing::warn!("refresh after add_custom_game failed: {e}");
                        }
                        DaemonMsg::Ok
                    }
                    Err(e) => err(e),
                }
            }
            GuiRequest::RemoveCustomGame { title } => {
                let norm = crate::naming::normalize_title(&title);
                match self.state.remove_custom_game(&norm).await {
                    Ok(()) => {
                        let _ = self.refresh_games(true).await;
                        DaemonMsg::Ok
                    }
                    Err(e) => err(e),
                }
            }
            // Shutdown is intercepted in serve_connection (it drains the whole
            // daemon, not just answers a reply), so it never reaches dispatch.
            GuiRequest::Shutdown => DaemonMsg::Ok,
        }
    }

    async fn add_root(&self, spec: RootSpec) -> DaemonMsg {
        match self.state.add_root(spec.kind, &spec.path).await {
            Ok(_) => {
                let _ = self.refresh_games(true).await;
                DaemonMsg::Ok
            }
            Err(e) => err(e),
        }
    }

    async fn backup_now(&self, game_id: GameId) -> DaemonMsg {
        let Some(entry) = self.games.read().await.get(&game_id).cloned() else {
            return DaemonMsg::Error {
                message: format!("unknown game {game_id}"),
            };
        };
        let device_id = self.device_id().await.unwrap_or_else(uuid::Uuid::nil);
        match run_backup(
            &self.state,
            Some(&self.client),
            device_id,
            self.config.full_every,
            &entry.backup_job(),
            &self.blob_cache,
        )
        .await
        {
            Ok(BackupOutcome::Conflict { head, incoming }) => {
                self.record_conflict(game_id, head.as_ref().map(|h| h.id), incoming.id)
                    .await
            }
            Ok(_) => DaemonMsg::Ok,
            Err(e) => err(e),
        }
    }

    async fn restore(&self, game_id: GameId, version_id: VersionId) -> DaemonMsg {
        let Some(entry) = self.games.read().await.get(&game_id).cloned() else {
            return DaemonMsg::Error {
                message: format!("unknown game {game_id}"),
            };
        };
        let running = self.running.read().await.contains(&game_id);
        let device_id = self.device_id().await.unwrap_or_else(uuid::Uuid::nil);
        let req = RestoreRequest {
            game_id,
            version_id,
            anchor: entry.resolved.anchor.clone(),
            patterns: entry.resolved.patterns.clone(),
            registry_keys: entry.resolved.registry_keys.clone(),
        };
        match run_restore(
            &self.state,
            &self.client,
            device_id,
            &req,
            running,
            &self.blob_cache,
            &self.scratch,
        )
        .await
        {
            Ok(()) => DaemonMsg::Ok,
            Err(e) => err(e),
        }
    }

    async fn resolve_conflict(&self, game_id: GameId, choice: ResolveChoice) -> DaemonMsg {
        let Some(tips) = self.conflicts.read().await.get(&game_id).copied() else {
            return DaemonMsg::Error {
                message: format!("no pending conflict for {game_id}"),
            };
        };
        let (winner, keep_both) = match choice {
            ResolveChoice::KeepMine => (tips.mine, false),
            ResolveChoice::KeepTheirs => (tips.theirs, false),
            ResolveChoice::KeepBoth => (tips.mine, true),
        };
        match self
            .client
            .resolve_conflict(game_id, &ResolveRequest { winner, keep_both })
            .await
        {
            Ok(()) => {
                self.conflicts.write().await.remove(&game_id);
                DaemonMsg::Ok
            }
            Err(e) => err(e),
        }
    }

    async fn update_config(&self, cfg: SyncedConfig) -> DaemonMsg {
        if let Err(e) = self.state.set_synced_config(&cfg).await {
            return err(e);
        }
        // Best-effort push to the server. On success, adopt the config the
        // server returns so our local `tag` matches the server's new one; if we
        // kept the old tag, the next edit would send a stale tag and be rejected
        // with a 409 and config would silently stop syncing.
        if self.client.is_authenticated().await {
            match self.client.put_config(&cfg).await {
                Ok(updated) => {
                    if let Err(e) = self.state.set_synced_config(&updated).await {
                        return err(e);
                    }
                }
                Err(e) => tracing::warn!("config push failed (kept locally): {e}"),
            }
        }
        let _ = self.refresh_games(true).await;
        DaemonMsg::Ok
    }

    async fn pair(&self, server_url: &str, code: &str, device_name: &str) -> DaemonMsg {
        let client = match ServerClient::new(server_url) {
            Ok(c) => c,
            Err(e) => return err(e),
        };
        match client.pair(code, device_name, Os::current()).await {
            Ok(paired) => {
                let creds = crate::secrets::Credentials {
                    device_id: paired.device_id,
                    account_id: paired.account_id,
                    refresh_secret: paired.refresh_secret,
                };
                if let Err(e) = self.secret_store.store(&creds) {
                    return err(e);
                }
                let _ = self.state.set_meta("server_url", server_url).await;
                *self.device_id.write().await = Some(paired.device_id);
                // Make the running client usable immediately (assumes the same
                // server_url; a different one is picked up on next restart).
                self.client.set_credentials(creds).await;
                self.client
                    .set_access_token(paired.access_token.clone())
                    .await;
                DaemonMsg::Paired {
                    device_id: paired.device_id,
                }
            }
            Err(e) => err(e),
        }
    }

    async fn status(&self) -> DaemonMsg {
        let pending_outbox = self.state.outbox_count().await.unwrap_or(0);
        let last_backup_at = self.state.last_backup_at().await.ok().flatten();
        DaemonMsg::Status(DaemonStatus {
            version: env!("CARGO_PKG_VERSION").to_string(),
            uptime_s: self.started_at.elapsed().as_secs(),
            rss_bytes: current_rss(),
            watched_games: self.games.read().await.len() as u32,
            server_connected: self.server_connected.load(Ordering::Relaxed)
                || self.client.is_authenticated().await,
            last_backup_at,
            pending_outbox,
            autostart_enabled: crate::autostart::is_enabled(),
        })
    }

    /// Record a conflict for later resolution and surface it to the GUI.
    async fn record_conflict(
        &self,
        game_id: GameId,
        theirs: Option<VersionId>,
        mine: VersionId,
    ) -> DaemonMsg {
        let theirs = theirs.unwrap_or(mine);
        self.conflicts
            .write()
            .await
            .insert(game_id, ConflictTips { mine, theirs });
        DaemonMsg::ConflictRaised {
            game_id,
            tips: vec![theirs, mine],
        }
    }

    // ---- event-driven backups (PRD-03 §2) ----

    /// Consume detection events: track running games and trigger backups on
    /// `GameStopped` / `ManualBackupRequested`. Runs until `shutdown` flips.
    pub async fn run_event_loop(self: Arc<Self>, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        let mut rx = self.subscribe();
        loop {
            tokio::select! {
                res = shutdown.changed() => {
                    if res.is_err() || *shutdown.borrow() { return; }
                }
                msg = rx.recv() => {
                    match msg {
                        Ok(event) => {
                            // Back up OFF the event loop: a slow save upload must
                            // not stall detection of the next launch (the live
                            // running flag) behind it.
                            if let Some(game_id) = self.on_event(event).await {
                                let engine = self.clone();
                                tokio::spawn(async move { engine.trigger_backup(game_id).await; });
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("event loop lagged, dropped {n} events");
                        }
                        Err(broadcast::error::RecvError::Closed) => return,
                    }
                }
            }
        }
    }

    /// Handle one detection event. Returns a game whose save should be backed
    /// up; the caller spawns that off the loop so a slow upload never stalls the
    /// next event. Fast, ordered in-memory + DB work stays inline here.
    async fn on_event(&self, event: DetectionEvent) -> Option<GameId> {
        match event {
            DetectionEvent::GameStarted { game_id, at, .. } => {
                // Persist the play row BEFORE flipping the in-memory running flag,
                // so any ListGames that observes running=true also sees the row
                // (never "Playing" next to "Last played: never").
                let _ = self.state.play_start(game_id, at).await;
                self.running.write().await.insert(game_id);
                // Nudge the GUI so the game lights up as "Playing" right away.
                let _ = self.events.send(DetectionEvent::CatalogUpdated);
                None
            }
            DetectionEvent::GameStopped { game_id, at } => {
                // Record the finished session before clearing the running flag,
                // for the same read-consistency reason.
                let _ = self.state.play_stop(game_id, at).await;
                self.running.write().await.remove(&game_id);
                let _ = self.events.send(DetectionEvent::CatalogUpdated);
                Some(game_id)
            }
            DetectionEvent::ManualBackupRequested { game_id } => Some(game_id),
            DetectionEvent::SaveDirChanged { .. } => {
                // Live mid-session backup is off by default (PRD-02 §5, G5).
                None
            }
            // The engine emits these for the app (toasts / reload); loop ignores.
            DetectionEvent::BackupCompleted { .. }
            | DetectionEvent::BackupConflict { .. }
            | DetectionEvent::SaveAvailable { .. }
            | DetectionEvent::CatalogUpdated => None,
        }
    }

    async fn trigger_backup(&self, game_id: GameId) {
        let Some(entry) = self.games.read().await.get(&game_id).cloned() else {
            return;
        };
        let device_id = self.device_id().await.unwrap_or_else(uuid::Uuid::nil);
        match run_backup(
            &self.state,
            Some(&self.client),
            device_id,
            self.config.full_every,
            &entry.backup_job(),
            &self.blob_cache,
        )
        .await
        {
            Ok(BackupOutcome::Uploaded { version }) => {
                let _ = self
                    .events
                    .send(DetectionEvent::BackupCompleted { game_id });
                tracing::info!("backed up {} -> version {}", entry.game.title, version.id);
            }
            Ok(BackupOutcome::Conflict { head, incoming }) => {
                let _ = self
                    .record_conflict(game_id, head.as_ref().map(|h| h.id), incoming.id)
                    .await;
                let _ = self.events.send(DetectionEvent::BackupConflict { game_id });
            }
            Ok(BackupOutcome::Queued) => {
                tracing::info!("{} queued for upload (offline)", entry.game.title);
            }
            Ok(BackupOutcome::NoChange) => {}
            Err(e) => tracing::error!("backup of {} failed: {e}", entry.game.title),
        }
    }
}

impl Engine {
    /// React to a server `version_available` push (PRD-03 §5). Auto-pulls when
    /// policy allows and it's safe; otherwise notifies the user.
    pub async fn on_version_available(&self, game_id: GameId, version_id: VersionId) {
        let running = self.running.read().await.contains(&game_id);
        let policy = self
            .games
            .read()
            .await
            .get(&game_id)
            .and_then(|e| {
                self.config
                    .games
                    .get(&e.game.title)
                    .and_then(|g| g.autopull)
            })
            .unwrap_or(self.config.autopull);

        match policy {
            // auto: pull iff the game isn't running (PRD-03 §5). The
            // local-unchanged guard is enforced inside restore's pre-backup.
            savr_core::AutoPullPolicy::Auto if !running => {
                if let DaemonMsg::Error { message } = self.restore(game_id, version_id).await {
                    tracing::warn!("auto-pull restore failed: {message}");
                }
            }
            _ => {
                let _ = self.events.send(DetectionEvent::SaveAvailable { game_id });
            }
        }
    }

    /// Record a conflict pushed over WebSocket (PRD-04 §4).
    pub async fn record_ws_conflict(&self, game_id: GameId, tips: Vec<VersionId>) {
        if let (Some(&theirs), Some(&mine)) = (tips.first(), tips.get(1)) {
            self.conflicts
                .write()
                .await
                .insert(game_id, ConflictTips { mine, theirs });
        }
        let _ = self.events.send(DetectionEvent::BackupConflict { game_id });
    }

    /// Retry any queued uploads against the server (PRD-03 §8).
    pub async fn flush_outbox(&self) {
        if let Err(e) =
            crate::backup::retry_outbox(&self.state, &self.client, &self.blob_cache).await
        {
            tracing::warn!("outbox flush error: {e}");
        }
    }

    /// Register the OS's default Steam roots if the user has none yet
    /// (first-boot convenience, PRD-07 §4).
    pub async fn ensure_default_roots(&self) -> anyhow::Result<()> {
        if !self.state.list_roots().await?.is_empty() {
            return Ok(());
        }
        for root in steam::default_steam_roots() {
            self.state
                .add_root(RootKind::Steam, &root.to_string_lossy())
                .await?;
            tracing::info!("registered default Steam root {}", root.display());
        }
        Ok(())
    }

    /// Pull the synced config after a `config_updated` push (PRD-04 §4).
    pub async fn pull_config(&self) -> anyhow::Result<()> {
        if !self.client.is_authenticated().await {
            return Ok(());
        }
        let cfg = self.client.get_config().await?;
        self.state.set_synced_config(&cfg).await?;
        self.refresh_games(true).await?;
        Ok(())
    }
}

fn err(e: impl std::fmt::Display) -> DaemonMsg {
    DaemonMsg::Error {
        message: e.to_string(),
    }
}

/// Resident set size of this process, best-effort (0 if unavailable). Powers
/// the GUI's "prove it's tiny" status (PRD-07 §6, G5).
fn current_rss() -> u64 {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let Ok(pid) = sysinfo::get_current_pid() else {
        return 0;
    };
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    sys.process(Pid::from(pid.as_u32() as usize))
        .map(|p| p.memory())
        .unwrap_or(0)
}
