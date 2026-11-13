//! BitTorrent backend.
//!
//! `librqbit` is async, but the rest of FlowDown is plain Win32 + blocking
//! threads. A dedicated background thread owns a small Tokio runtime and the
//! torrent session; the UI talks to it through a lock-free command channel.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use librqbit::api::TorrentIdOrHash;
use librqbit::{
    AddTorrent, AddTorrentOptions, DhtSessionConfig, ListenerOptions, ManagedTorrent, Session,
    SessionOptions, TorrentStatsState,
};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::sync::Mutex as AsyncMutex;

use crate::download::{update_task, PeerInfo, TaskStatus, TaskStore};
use crate::{tr, trf};

/// Public trackers added to every non-private torrent. A magnet link without
/// `tr=` parameters can only find peers through DHT, which is unreliable on
/// many networks (especially behind NAT or with UDP filtering). Trackers give
/// metadata resolution a second, usually faster, source of peers.
const DEFAULT_TRACKERS: &[&str] = &[
    "udp://tracker.opentrackr.org:1337/announce",
    "http://tracker.opentrackr.org:1337/announce",
    "https://tracker.opentrackr.org:443/announce",
    "udp://open.demonii.com:1337/announce",
    "udp://tracker.torrent.eu.org:451/announce",
    "https://tracker.torrent.eu.org/announce",
    "http://open.tracker.cl:1337/announce",
    "http://tracker.dler.org:6969/announce",
];

/// How long to wait for magnet metadata (the file list) before giving up.
/// Without this, a magnet whose swarm is unreachable stays in "connecting"
/// forever with no feedback.
const METADATA_TIMEOUT: Duration = Duration::from_secs(90);

enum Command {
    Add {
        id: u64,
        url: String,
        dir: PathBuf,
        start: bool,
    },
    Pause {
        id: u64,
    },
    Remove {
        id: u64,
        delete_files: bool,
    },
    SetDir(PathBuf),
    Shutdown,
}

/// Handle used by `DownloadManager`. `Send + Sync`, no Tokio types leak out.
pub struct TorrentBackend {
    tx: UnboundedSender<Command>,
}

impl TorrentBackend {
    pub fn start(store: TaskStore, default_dir: PathBuf) -> Self {
        let (tx, mut rx) = unbounded_channel::<Command>();
        let _ = std::thread::Builder::new()
            .name("flowdown-bt".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .thread_name("flowdown-bt-worker")
                    .build()
                {
                    Ok(rt) => rt,
                    Err(_) => return,
                };
                runtime.block_on(async move {
                    let options = SessionOptions {
                        // Keep DHT peer discovery, but don't drop cache files in
                        // whatever happens to be the current directory.
                        dht: Some(DhtSessionConfig {
                            persistence: None,
                            ..Default::default()
                        }),
                        // Accept incoming peer connections. Outgoing-only
                        // sessions miss every peer that only connects to
                        // listening clients.
                        listen: Some(ListenerOptions {
                            listen_addr: SocketAddr::from(([0, 0, 0, 0], 0)),
                            ipv4_only: true,
                            ..Default::default()
                        }),
                        // Session-wide fallback trackers for trackers-less
                        // magnet links. librqbit ignores these for private
                        // torrents, so they can't break a private swarm.
                        trackers: DEFAULT_TRACKERS
                            .iter()
                            .filter_map(|tracker| url::Url::parse(tracker).ok())
                            .collect(),
                        ..Default::default()
                    };
                    let session = match Session::new_with_opts(default_dir.clone(), options).await {
                        Ok(session) => session,
                        Err(_) => return,
                    };
                    let shared = Arc::new(Shared {
                        session: session.clone(),
                        store,
                        handles: AsyncMutex::new(HashMap::new()),
                        paused: Mutex::new(HashSet::new()),
                        adding: Mutex::new(HashSet::new()),
                        removed: Mutex::new(HashSet::new()),
                        dir: Mutex::new(default_dir),
                    });
                    while let Some(cmd) = rx.recv().await {
                        match cmd {
                            Command::Add {
                                id,
                                url,
                                dir,
                                start,
                            } => {
                                {
                                    let handles = shared.handles.lock().await;
                                    if let Some(handle) = handles.get(&id).cloned() {
                                        drop(handles);
                                        if start {
                                            if let Ok(mut paused) = shared.paused.lock() {
                                                paused.remove(&id);
                                            }
                                            let _ = shared.session.unpause(&handle).await;
                                        }
                                        continue;
                                    }
                                }
                                let fresh = shared
                                    .adding
                                    .lock()
                                    .map(|mut set| set.insert(id))
                                    .unwrap_or(false);
                                if !fresh {
                                    // An add for this row is already resolving
                                    // metadata; just honor the requested state.
                                    if let Ok(mut paused) = shared.paused.lock() {
                                        if start {
                                            paused.remove(&id);
                                        } else {
                                            paused.insert(id);
                                        }
                                    }
                                    continue;
                                }
                                if let Ok(mut removed) = shared.removed.lock() {
                                    removed.remove(&id);
                                }
                                if !start {
                                    if let Ok(mut paused) = shared.paused.lock() {
                                        paused.insert(id);
                                    }
                                }
                                tokio::spawn(add_torrent(shared.clone(), id, url, dir, start));
                            }
                            Command::Pause { id } => {
                                if let Ok(mut paused) = shared.paused.lock() {
                                    paused.insert(id);
                                }
                                let handle = shared.handles.lock().await.get(&id).cloned();
                                if let Some(handle) = handle {
                                    let _ = shared.session.pause(&handle).await;
                                }
                            }
                            Command::Remove { id, delete_files } => {
                                if let Ok(mut paused) = shared.paused.lock() {
                                    paused.remove(&id);
                                }
                                if let Ok(mut removed) = shared.removed.lock() {
                                    removed.insert(id);
                                }
                                let handle = shared.handles.lock().await.remove(&id);
                                if let Some(handle) = handle {
                                    let _ = shared
                                        .session
                                        .delete(TorrentIdOrHash::Id(handle.id()), delete_files)
                                        .await;
                                }
                            }
                            Command::SetDir(dir) => {
                                if let Ok(mut current) = shared.dir.lock() {
                                    *current = dir;
                                }
                            }
                            Command::Shutdown => break,
                        }
                    }
                    session.cancellation_token().cancel();
                });
            });
        Self { tx }
    }

    pub fn add(&self, id: u64, url: &str, dir: PathBuf, start: bool) -> Result<(), String> {
        self.tx
            .send(Command::Add {
                id,
                url: url.to_owned(),
                dir,
                start,
            })
            .map_err(|_| {
                tr!(
                    "the BitTorrent engine is not running",
                    "BitTorrent 引擎未运行"
                )
                .to_owned()
            })
    }

    pub fn pause(&self, id: u64) -> Result<(), String> {
        self.tx.send(Command::Pause { id }).map_err(|_| {
            tr!(
                "the BitTorrent engine is not running",
                "BitTorrent 引擎未运行"
            )
            .to_owned()
        })
    }

    pub fn remove(&self, id: u64) -> Result<(), String> {
        self.tx
            .send(Command::Remove {
                id,
                delete_files: false,
            })
            .map_err(|_| {
                tr!(
                    "the BitTorrent engine is not running",
                    "BitTorrent 引擎未运行"
                )
                .to_owned()
            })
    }

    pub fn set_dir(&self, dir: PathBuf) {
        let _ = self.tx.send(Command::SetDir(dir));
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(Command::Shutdown);
    }
}

struct Shared {
    session: Arc<Session>,
    store: TaskStore,
    handles: AsyncMutex<HashMap<u64, Arc<ManagedTorrent>>>,
    paused: Mutex<HashSet<u64>>,
    adding: Mutex<HashSet<u64>>,
    removed: Mutex<HashSet<u64>>,
    dir: Mutex<PathBuf>,
}

impl Shared {
    fn is_paused(&self, id: u64) -> bool {
        self.paused
            .lock()
            .map(|paused| paused.contains(&id))
            .unwrap_or(false)
    }

    fn is_removed(&self, id: u64) -> bool {
        self.removed
            .lock()
            .map(|removed| removed.contains(&id))
            .unwrap_or(true)
    }
}

async fn add_torrent(shared: Arc<Shared>, id: u64, url: String, dir: PathBuf, start: bool) {
    let output_folder = dir.to_string_lossy().into_owned();
    let options = AddTorrentOptions {
        overwrite: true,
        paused: !start,
        output_folder: Some(output_folder),
        ..Default::default()
    };
    let session = shared.session.clone();
    let add = session.add_torrent(AddTorrent::from_url(url), Some(options));
    let result = tokio::time::timeout(METADATA_TIMEOUT, add).await;
    if let Ok(mut adding) = shared.adding.lock() {
        adding.remove(&id);
    }
    let handle = match result {
        Ok(Ok(response)) => match response.into_handle() {
            Some(handle) => handle,
            None => {
                set_error(
                    &shared.store,
                    id,
                    tr!(
                        "The torrent has no downloadable files.",
                        "该种子没有可下载的文件。"
                    ),
                );
                return;
            }
        },
        Ok(Err(error)) => {
            if shared.is_removed(id) {
                return;
            }
            set_error(
                &shared.store,
                id,
                &trf!(
                    "Could not add torrent: {error:#}",
                    "无法添加种子：{error:#}"
                ),
            );
            return;
        }
        Err(_) => {
            if shared.is_removed(id) {
                return;
            }
            set_error(
                &shared.store,
                id,
                tr!(
                    "Timed out resolving the torrent: no peers or trackers responded. \
                     This swarm may only be reachable from clients such as Xunlei.",
                    "解析种子信息超时：没有 peer 或 tracker 响应。该资源可能只能被迅雷等客户端下载。"
                ),
            );
            return;
        }
    };
    if shared.is_removed(id) {
        let _ = shared
            .session
            .delete(TorrentIdOrHash::Id(handle.id()), false)
            .await;
        return;
    }
    shared.handles.lock().await.insert(id, handle.clone());
    apply_metadata(&shared.store, id, &handle);
    if shared.is_paused(id) {
        let _ = shared.session.pause(&handle).await;
    }
    tokio::spawn(monitor(shared, id, handle));
}

async fn monitor(shared: Arc<Shared>, id: u64, handle: Arc<ManagedTorrent>) {
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if shared.is_removed(id) || !task_exists(&shared.store, id) {
            break;
        }
        let stats = handle.stats();
        let paused = shared.is_paused(id);
        let name = handle.name();
        let folder = handle.output_folder().to_path_buf();
        let dest = handle
            .with_metadata(|metadata| {
                metadata
                    .file_infos
                    .first()
                    .map(|file| folder.join(&file.relative_filename))
            })
            .ok()
            .flatten();
        let finished =
            stats.finished || (stats.total_bytes > 0 && stats.progress_bytes >= stats.total_bytes);
        let state = stats.state;
        let error = stats.error.clone();
        let total = stats.total_bytes;
        let done = stats.progress_bytes;
        let peer_info = stats.live.as_ref().map(|live| PeerInfo {
            seeders: live.snapshot.peer_stats.live_seeders,
            peers: live.snapshot.peer_stats.live,
            seen: live.snapshot.peer_stats.seen,
        });

        update_task(&shared.store, id, |task| {
            task.progress.set_total(total);
            task.progress.set_done(done);
            task.peer_info = peer_info;
            if let Some(name) = name.as_ref().filter(|name| !name.is_empty()) {
                task.name = name.clone();
            }
            if task.dest_path.is_none() {
                if let Some(dest) = dest.clone() {
                    task.dest_path = Some(dest);
                }
            }
            if paused {
                if !finished {
                    task.status = TaskStatus::Paused;
                }
                return;
            }
            if finished {
                task.status = TaskStatus::Completed;
                task.error = None;
                return;
            }
            match state {
                TorrentStatsState::Initializing { .. } => {
                    task.status = TaskStatus::Connecting;
                }
                TorrentStatsState::Live => {
                    task.status = TaskStatus::Downloading;
                }
                TorrentStatsState::Paused => {
                    task.status = TaskStatus::Paused;
                }
                TorrentStatsState::Error => {
                    task.status = TaskStatus::Error;
                    task.error = error.clone();
                }
            }
        });

        if finished || matches!(state, TorrentStatsState::Error) {
            break;
        }
    }
}

fn apply_metadata(store: &TaskStore, id: u64, handle: &ManagedTorrent) {
    let name = handle.name();
    let folder = handle.output_folder().to_path_buf();
    let dest = handle
        .with_metadata(|metadata| {
            metadata
                .file_infos
                .first()
                .map(|file| folder.join(&file.relative_filename))
        })
        .ok()
        .flatten();
    update_task(store, id, |task| {
        if let Some(name) = name.as_ref().filter(|name| !name.is_empty()) {
            task.name = name.clone();
        }
        if dest.is_some() {
            task.dest_path = dest;
        }
    });
}

fn set_error(store: &TaskStore, id: u64, message: &str) {
    update_task(store, id, |task| {
        task.status = TaskStatus::Error;
        task.error = Some(message.to_owned());
    });
}

fn task_exists(store: &TaskStore, id: u64) -> bool {
    store
        .lock()
        .map(|tasks| tasks.iter().any(|task| task.id == id))
        .unwrap_or(false)
}
