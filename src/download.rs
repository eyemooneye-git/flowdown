//! Download engine: segmented HTTP/HTTPS and FTP, plus a command channel to
//! the BitTorrent backend.
//!
//! Every transfer has persistent `.part` files, so pausing or a transient
//! network failure keeps already received bytes. Segments are merged only
//! after every part has reached its expected length.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use suppaftp::types::FileType;
use suppaftp::FtpStream;
use url::Url;

use crate::torrent::TorrentBackend;
use crate::util::{self, format_size, unique_path};
use crate::{tr, trf};

pub type TaskStore = Arc<Mutex<Vec<Task>>>;

const USER_AGENT: &str = "FlowDown/1.0";
const MIN_SEGMENT_BYTES: u64 = 512 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskKind {
    Http,
    Ftp,
    Torrent,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskStatus {
    Queued,
    Connecting,
    Downloading,
    Paused,
    Completed,
    Error,
}

impl TaskStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => tr!("Queued", "等待中"),
            Self::Connecting => tr!("Connecting", "连接中"),
            Self::Downloading => tr!("Downloading", "下载中"),
            Self::Paused => tr!("Paused", "已暂停"),
            Self::Completed => tr!("Completed", "已完成"),
            Self::Error => tr!("Error", "错误"),
        }
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Connecting | Self::Downloading)
    }
}

#[derive(Default)]
pub struct Progress {
    done: AtomicU64,
    total: AtomicU64,
}

impl Progress {
    pub fn done(&self) -> u64 {
        self.done.load(Ordering::Relaxed)
    }
    pub fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }
    pub fn set_done(&self, v: u64) {
        self.done.store(v, Ordering::Relaxed);
    }
    pub fn set_total(&self, v: u64) {
        self.total.store(v, Ordering::Relaxed);
    }
    pub fn add_done(&self, v: u64) -> u64 {
        self.done.fetch_add(v, Ordering::Relaxed) + v
    }
}

/// Live seed/peer counts reported by the BitTorrent backend.
#[derive(Clone, Copy, Default, Debug)]
pub struct PeerInfo {
    pub seeders: u32,
    pub peers: u32,
    pub seen: u32,
}

#[derive(Default)]
pub struct Control {
    cancel: AtomicBool,
    pause: AtomicBool,
}

impl Control {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn pause(&self) {
        self.pause.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    pub fn is_paused(&self) -> bool {
        self.pause.load(Ordering::Relaxed)
    }
    fn stop(&self) -> Stop {
        if self.is_cancelled() {
            Stop::Cancel
        } else if self.is_paused() {
            Stop::Pause
        } else {
            Stop::None
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    None,
    Pause,
    Cancel,
}

pub struct Task {
    pub id: u64,
    pub url: String,
    pub display_url: String,
    pub name: String,
    pub kind: TaskKind,
    pub status: TaskStatus,
    pub error: Option<String>,
    pub dest_dir: PathBuf,
    pub dest_path: Option<PathBuf>,
    pub connections: usize,
    pub progress: Arc<Progress>,
    pub control: Arc<Control>,
    pub peer_info: Option<PeerInfo>,
}

impl Task {
    fn view(&self) -> TaskView {
        TaskView {
            id: self.id,
            url: self.url.clone(),
            display_url: self.display_url.clone(),
            name: self.name.clone(),
            status: self.status,
            error: self.error.clone(),
            dest_dir: self.dest_dir.clone(),
            dest_path: self.dest_path.clone(),
            total: self.progress.total(),
            done: self.progress.done(),
            peer_info: self.peer_info,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TaskView {
    pub id: u64,
    pub url: String,
    pub display_url: String,
    pub name: String,
    pub status: TaskStatus,
    pub error: Option<String>,
    pub dest_dir: PathBuf,
    pub dest_path: Option<PathBuf>,
    pub total: u64,
    pub done: u64,
    pub peer_info: Option<PeerInfo>,
}

impl TaskView {
    pub fn progress(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f64 / self.total as f64).clamp(0.0, 1.0)
        }
    }

    pub fn progress_text(&self) -> String {
        if self.status == TaskStatus::Completed {
            return "100%".to_owned();
        }
        if self.total == 0 {
            if self.done > 0 {
                format_size(self.done)
            } else {
                "—".to_owned()
            }
        } else {
            format!("{:.1}%", self.progress() * 100.0)
        }
    }

    pub fn size_text(&self) -> String {
        if self.total == 0 {
            if self.done > 0 {
                format_size(self.done)
            } else {
                "—".to_owned()
            }
        } else {
            format_size(self.total)
        }
    }

    pub fn detail(&self) -> String {
        match self.status {
            TaskStatus::Error => self
                .error
                .clone()
                .unwrap_or_else(|| tr!("Download failed", "下载失败").to_owned()),
            TaskStatus::Completed => self
                .dest_path
                .as_ref()
                .map(|p| {
                    tr!(
                        format!("Saved to {}", p.display()),
                        format!("已保存到 {}", p.display())
                    )
                })
                .unwrap_or_else(|| tr!("Completed", "已完成").to_owned()),
            TaskStatus::Paused => tr!("Paused", "已暂停").to_owned(),
            TaskStatus::Queued => tr!("Waiting to start", "等待开始").to_owned(),
            TaskStatus::Connecting => tr!("Connecting…", "正在连接…").to_owned(),
            TaskStatus::Downloading => {
                if self.total > 0 {
                    format!("{} / {}", format_size(self.done), format_size(self.total))
                } else {
                    tr!(
                        format!("{} received", format_size(self.done)),
                        format!("已接收 {}", format_size(self.done))
                    )
                }
            }
        }
    }
}

pub struct DownloadManager {
    store: TaskStore,
    next_id: AtomicU64,
    download_dir: Mutex<PathBuf>,
    connections: AtomicUsize,
    torrent: Mutex<Option<TorrentBackend>>,
    shutting_down: AtomicBool,
}

impl DownloadManager {
    pub fn new(download_dir: PathBuf, connections: usize) -> Self {
        let _ = fs::create_dir_all(&download_dir);
        Self {
            store: Arc::new(Mutex::new(Vec::new())),
            next_id: AtomicU64::new(1),
            download_dir: Mutex::new(download_dir),
            connections: AtomicUsize::new(connections.clamp(1, 32)),
            torrent: Mutex::new(None),
            shutting_down: AtomicBool::new(false),
        }
    }

    pub fn set_connections(&self, n: usize) {
        self.connections.store(n.clamp(1, 32), Ordering::Relaxed);
    }

    pub fn set_download_dir(&self, dir: PathBuf) {
        let _ = fs::create_dir_all(&dir);
        if let Ok(mut g) = self.download_dir.lock() {
            *g = dir.clone();
        }
        if let Ok(g) = self.torrent.lock() {
            if let Some(t) = g.as_ref() {
                t.set_dir(dir);
            }
        }
    }

    pub fn download_dir(&self) -> PathBuf {
        self.download_dir
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|_| crate::settings::default_download_dir())
    }

    pub fn add_link(&self, raw: &str, start: bool) -> Result<u64, String> {
        let url = raw.trim();
        if url.is_empty() {
            return Err(tr!("Enter a link first.", "请先输入链接。").to_owned());
        }
        let kind = match util::link_kind(url) {
            Some(util::LinkKind::Http) => TaskKind::Http,
            Some(util::LinkKind::Ftp) => TaskKind::Ftp,
            Some(util::LinkKind::Torrent) => TaskKind::Torrent,
            None => {
                return Err(tr!(
                    "Only HTTP, HTTPS, FTP and magnet links are supported.",
                    "仅支持 HTTP、HTTPS、FTP 和磁力链接。"
                )
                .to_owned())
            }
        };
        let name = match kind {
            TaskKind::Torrent => util::magnet_name(url)
                .unwrap_or_else(|| tr!("Magnet download", "磁力链接下载").to_owned()),
            _ => util::filename_from_url(url),
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let options = self.connections.load(Ordering::Relaxed).clamp(1, 32);
        let task = Task {
            id,
            url: url.to_owned(),
            display_url: util::display_url(url),
            name,
            kind,
            status: TaskStatus::Queued,
            error: None,
            dest_dir: self.download_dir(),
            dest_path: None,
            connections: options,
            progress: Arc::new(Progress::default()),
            control: Arc::new(Control::default()),
            peer_info: None,
        };
        if let Ok(mut tasks) = self.store.lock() {
            tasks.push(task);
        }
        if start {
            if let Err(e) = self.start(id) {
                update_task(&self.store, id, |t| {
                    t.status = TaskStatus::Error;
                    t.error = Some(e);
                });
            }
        }
        Ok(id)
    }

    pub fn start(&self, id: u64) -> Result<(), String> {
        enum Plan {
            Worker(Job, Arc<Control>, Arc<Progress>),
            Torrent { url: String, dir: PathBuf },
        }
        let plan;
        {
            let mut tasks = self
                .store
                .lock()
                .map_err(|_| tr!("state lock poisoned", "下载状态已被锁定"))?;
            let Some(task) = tasks.iter_mut().find(|t| t.id == id) else {
                return Err(tr!("Task no longer exists.", "任务已不存在。").to_owned());
            };
            match task.kind {
                TaskKind::Torrent => {
                    if matches!(
                        task.status,
                        TaskStatus::Connecting | TaskStatus::Downloading | TaskStatus::Completed
                    ) {
                        return Ok(());
                    }
                    task.status = TaskStatus::Connecting;
                    task.error = None;
                    plan = Some(Plan::Torrent {
                        url: task.url.clone(),
                        dir: task.dest_dir.clone(),
                    });
                }
                TaskKind::Http | TaskKind::Ftp => {
                    if matches!(
                        task.status,
                        TaskStatus::Connecting | TaskStatus::Downloading | TaskStatus::Completed
                    ) {
                        return Ok(());
                    }
                    let control = Arc::new(Control::default());
                    let progress = Arc::new(Progress::default());
                    task.control = control.clone();
                    task.progress = progress.clone();
                    task.status = TaskStatus::Connecting;
                    task.error = None;
                    let job = Job {
                        id: task.id,
                        url: task.url.clone(),
                        kind: task.kind,
                        name: task.name.clone(),
                        dest_dir: task.dest_dir.clone(),
                        dest_path: task.dest_path.clone(),
                        connections: task.connections.clamp(1, 32),
                    };
                    plan = Some(Plan::Worker(job, control, progress));
                }
            }
        }
        match plan {
            Some(Plan::Worker(job, control, progress)) => {
                let store = self.store.clone();
                let name = format!("flowdown-{}", job.id);
                let _ = std::thread::Builder::new().name(name).spawn(move || {
                    run_job(job, store, control, progress);
                });
                Ok(())
            }
            Some(Plan::Torrent { url, dir }) => {
                let backend = self.ensure_torrent()?;
                let backend = backend.as_ref().ok_or_else(|| {
                    tr!("BitTorrent backend unavailable", "BitTorrent 后端不可用")
                })?;
                backend.add(id, &url, dir, true).map_err(|e| {
                    tr!(
                        format!("BitTorrent backend unavailable: {e}"),
                        format!("BitTorrent 后端不可用：{e}")
                    )
                })
            }
            None => Ok(()),
        }
    }

    pub fn pause(&self, id: u64) {
        let mut torrent = None;
        if let Ok(mut tasks) = self.store.lock() {
            if let Some(task) = tasks.iter_mut().find(|t| t.id == id) {
                match task.kind {
                    TaskKind::Torrent => {
                        if task.status != TaskStatus::Completed {
                            task.status = TaskStatus::Paused;
                            torrent = Some(());
                        }
                    }
                    _ => {
                        task.control.pause();
                        if matches!(
                            task.status,
                            TaskStatus::Connecting | TaskStatus::Downloading | TaskStatus::Queued
                        ) {
                            task.status = TaskStatus::Paused;
                        }
                    }
                }
            }
        }
        if torrent.is_some() {
            if let Ok(g) = self.torrent.lock() {
                if let Some(t) = g.as_ref() {
                    let _ = t.pause(id);
                }
            }
        }
    }

    pub fn resume(&self, id: u64) {
        let _ = self.start(id);
    }

    pub fn remove(&self, id: u64) {
        let mut torrent_id = None;
        let mut cleanup = None;
        let mut cancel_ctrl = None;
        if let Ok(mut tasks) = self.store.lock() {
            if let Some(pos) = tasks.iter().position(|t| t.id == id) {
                let task = tasks.remove(pos);
                if task.kind == TaskKind::Torrent {
                    torrent_id = Some(());
                } else {
                    cleanup = Some((
                        task.dest_path.clone(),
                        task.id,
                        task.connections,
                        task.status,
                    ));
                    cancel_ctrl = Some(task.control);
                }
            }
        }
        if let Some(ctrl) = cancel_ctrl {
            ctrl.cancel();
        }
        if let Some((Some(dest), tid, connections, _)) = cleanup {
            // `.part` files (and any merge temp) are ours to remove; a final
            // destination file that already exists is left in place.
            let _ = cleanup_parts(&dest, tid, connections.max(64));
        }
        if torrent_id.is_some() {
            if let Ok(g) = self.torrent.lock() {
                if let Some(t) = g.as_ref() {
                    let _ = t.remove(id);
                }
            }
        }
    }

    pub fn clear_finished(&self) {
        let ids: Vec<u64> = self
            .store
            .lock()
            .map(|t| {
                t.iter()
                    .filter(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Error))
                    .map(|t| t.id)
                    .collect()
            })
            .unwrap_or_default();
        for id in ids {
            self.remove(id);
        }
    }

    pub fn pause_all(&self) {
        let ids: Vec<u64> = self
            .store
            .lock()
            .map(|t| {
                t.iter()
                    .filter(|t| {
                        matches!(
                            t.status,
                            TaskStatus::Connecting | TaskStatus::Downloading | TaskStatus::Queued
                        )
                    })
                    .map(|t| t.id)
                    .collect()
            })
            .unwrap_or_default();
        for id in ids {
            self.pause(id);
        }
    }

    pub fn resume_all(&self) {
        let ids: Vec<u64> = self
            .store
            .lock()
            .map(|t| {
                t.iter()
                    .filter(|t| {
                        matches!(
                            t.status,
                            TaskStatus::Paused | TaskStatus::Queued | TaskStatus::Error
                        )
                    })
                    .map(|t| t.id)
                    .collect()
            })
            .unwrap_or_default();
        for id in ids {
            self.resume(id);
        }
    }

    pub fn snapshot(&self) -> Vec<TaskView> {
        self.store
            .lock()
            .map(|tasks| tasks.iter().map(Task::view).collect())
            .unwrap_or_default()
    }

    pub fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Relaxed);
        if let Ok(tasks) = self.store.lock() {
            for task in tasks.iter() {
                if task.status.is_active() {
                    task.control.cancel();
                }
            }
        }
        if let Ok(g) = self.torrent.lock() {
            if let Some(t) = g.as_ref() {
                t.shutdown();
            }
        }
    }

    fn ensure_torrent(&self) -> Result<std::sync::MutexGuard<'_, Option<TorrentBackend>>, String> {
        let mut guard = self
            .torrent
            .lock()
            .map_err(|_| tr!("BitTorrent lock poisoned", "BitTorrent 状态已被锁定").to_owned())?;
        if guard.is_none() {
            let dir = self.download_dir();
            *guard = Some(TorrentBackend::start(self.store.clone(), dir));
        }
        Ok(guard)
    }
}

impl Drop for DownloadManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Clone)]
struct Job {
    id: u64,
    url: String,
    kind: TaskKind,
    name: String,
    dest_dir: PathBuf,
    dest_path: Option<PathBuf>,
    connections: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RunEnd {
    Completed,
    Paused,
    Canceled,
}

impl RunEnd {
    fn from_stop(stop: Stop) -> Self {
        match stop {
            Stop::Cancel => Self::Canceled,
            Stop::Pause => Self::Paused,
            Stop::None => Self::Paused,
        }
    }
}

fn run_job(job: Job, store: TaskStore, control: Arc<Control>, progress: Arc<Progress>) {
    let result = match job.kind {
        TaskKind::Http => run_http(&job, &store, &control, &progress),
        TaskKind::Ftp => run_ftp(&job, &store, &control, &progress),
        TaskKind::Torrent => Ok(RunEnd::Completed),
    };
    match result {
        Ok(RunEnd::Completed) => {
            set_status_if_current(&store, job.id, &control, TaskStatus::Completed, None);
        }
        Ok(RunEnd::Paused) => {
            set_status_if_current(&store, job.id, &control, TaskStatus::Paused, None);
        }
        Ok(RunEnd::Canceled) => {
            cleanup_task_parts(&store, &job, &control);
            set_status_if_current(&store, job.id, &control, TaskStatus::Paused, None);
        }
        Err(e) => {
            if control.is_cancelled() {
                cleanup_task_parts(&store, &job, &control);
            } else if control.is_paused() {
                set_status_if_current(&store, job.id, &control, TaskStatus::Paused, None);
            } else {
                set_status_if_current(&store, job.id, &control, TaskStatus::Error, Some(e));
            }
        }
    }
}

fn set_status_if_current(
    store: &TaskStore,
    id: u64,
    control: &Arc<Control>,
    status: TaskStatus,
    error: Option<String>,
) {
    if let Ok(mut tasks) = store.lock() {
        if let Some(task) = tasks.iter_mut().find(|t| t.id == id) {
            if Arc::ptr_eq(&task.control, control) {
                task.status = status;
                task.error = error;
            }
        }
    }
}

pub fn update_task(store: &TaskStore, id: u64, f: impl FnOnce(&mut Task)) {
    if let Ok(mut tasks) = store.lock() {
        if let Some(task) = tasks.iter_mut().find(|t| t.id == id) {
            f(task);
        }
    }
}

fn update_if_current(
    store: &TaskStore,
    id: u64,
    control: &Arc<Control>,
    f: impl FnOnce(&mut Task),
) {
    if let Ok(mut tasks) = store.lock() {
        if let Some(task) = tasks.iter_mut().find(|t| t.id == id) {
            if Arc::ptr_eq(&task.control, control) {
                f(task);
            }
        }
    }
}

fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(45))
        .timeout_write(Duration::from_secs(45))
        .redirects(10)
        .build()
}

struct HttpProbe {
    final_url: String,
    name: Option<String>,
    total: Option<u64>,
    ranges: bool,
}

fn run_http(
    job: &Job,
    store: &TaskStore,
    control: &Arc<Control>,
    progress: &Arc<Progress>,
) -> Result<RunEnd, String> {
    if let Stop::Cancel | Stop::Pause = control.stop() {
        return Ok(RunEnd::from_stop(control.stop()));
    }
    let agent = http_agent();
    let probe = http_probe(&agent, &job.url)?;
    if let Some(total) = probe.total {
        progress.set_total(total);
    }
    let name = probe.name.clone().unwrap_or_else(|| job.name.clone());
    let (dest, was_existing) = match job.dest_path.clone() {
        Some(p) => (p, true),
        None => (
            reserve_destination(store, job.id, &job.dest_dir, &name),
            false,
        ),
    };
    update_if_current(store, job.id, control, |t| {
        t.name = name.clone();
        if !was_existing {
            t.dest_path = Some(dest.clone());
        }
    });

    if let Stop::Cancel | Stop::Pause = control.stop() {
        return Ok(RunEnd::from_stop(control.stop()));
    }

    let dest = if !was_existing && dest.exists() {
        reserve_destination(store, job.id, &job.dest_dir, &name)
    } else {
        dest
    };
    if let Some(total) = probe.total {
        if total == 0 {
            if let Some(parent) = dest.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = File::create(&dest)
                .map_err(|e| trf!("Could not create file: {e}", "无法创建文件：{e}"))?;
            progress.set_done(0);
            update_if_current(store, job.id, control, |t| {
                t.dest_path = Some(dest.clone());
            });
            return Ok(RunEnd::Completed);
        }
        if probe.ranges {
            let segments = split_ranges(total, job.connections);
            let parts: Vec<PathBuf> = segments
                .iter()
                .enumerate()
                .map(|(i, _)| part_path(&dest, job.id, i))
                .collect();
            prepare_parts(&parts, &segments, progress)?;
            let agent = Arc::new(agent);
            let mut handles = Vec::new();
            for (i, (start, end)) in segments.iter().copied().enumerate() {
                let agent = agent.clone();
                let url = probe.final_url.clone();
                let part = parts[i].clone();
                let progress = progress.clone();
                let control = control.clone();
                handles.push(std::thread::spawn(move || {
                    http_part_retry(&url, &agent, start, end, &part, &progress, &control)
                }));
            }
            let mut pause = false;
            let mut cancel = false;
            let mut error: Option<String> = None;
            for h in handles {
                match h.join() {
                    Ok(Ok(End::Done)) => {}
                    Ok(Ok(End::Pause)) => pause = true,
                    Ok(Ok(End::Cancel)) => cancel = true,
                    Ok(Err(e)) => {
                        if error.is_none() {
                            error = Some(e);
                        }
                    }
                    Err(_) => {
                        if error.is_none() {
                            error = Some(
                                tr!("A download worker panicked.", "下载线程发生错误。").to_owned(),
                            );
                        }
                    }
                }
            }
            if cancel {
                return Ok(RunEnd::Canceled);
            }
            if pause {
                return Ok(RunEnd::Paused);
            }
            if let Some(e) = error {
                return Err(e);
            }
            for (i, (start, end)) in segments.iter().copied().enumerate() {
                let expected = end - start + 1;
                let actual = fs::metadata(&parts[i]).map(|m| m.len()).unwrap_or(0);
                if actual != expected {
                    return Err(trf!(
                        "Segment {} is incomplete ({} / {} bytes).",
                        "第 {} 段不完整（{} / {} 字节）。",
                        i + 1,
                        actual,
                        expected
                    ));
                }
            }
            merge_parts(&dest, &parts, job.id)?;
            update_if_current(store, job.id, control, |t| {
                t.dest_path = Some(dest.clone());
            });
            Ok(RunEnd::Completed)
        } else {
            http_single(
                &probe.final_url,
                &agent,
                &dest,
                Some(total),
                job.id,
                progress,
                control,
            )
        }
    } else {
        http_single(
            &probe.final_url,
            &agent,
            &dest,
            None,
            job.id,
            progress,
            control,
        )
    }
}

fn http_probe(agent: &ureq::Agent, url: &str) -> Result<HttpProbe, String> {
    let resp = match agent
        .get(url)
        .set("Range", "bytes=0-0")
        .set("Accept-Encoding", "identity")
        .call()
    {
        Ok(r) => r,
        Err(ureq::Error::Status(416, r)) => {
            // Empty file or an unusual range response. Retry with a normal GET
            // just to get headers; the body is not consumed.
            let total = r
                .header("content-range")
                .and_then(content_range_total)
                .or_else(|| r.header("content-length").and_then(|v| v.parse().ok()));
            let final_url = r.get_url().to_owned();
            let name = r
                .header("content-disposition")
                .and_then(util::filename_from_disposition)
                .or_else(|| Some(util::filename_from_url(&final_url)));
            return Ok(HttpProbe {
                final_url,
                name,
                total: total.or(Some(0)),
                ranges: true,
            });
        }
        Err(ureq::Error::Status(code, _)) => {
            return Err(trf!(
                "Server returned HTTP {code}.",
                "服务器返回 HTTP {code}。"
            ));
        }
        Err(ureq::Error::Transport(e)) => {
            return Err(trf!("Could not connect: {e}", "无法连接：{e}"));
        }
    };
    let status = resp.status();
    let final_url = resp.get_url().to_owned();
    let name = resp
        .header("content-disposition")
        .and_then(util::filename_from_disposition)
        .or_else(|| Some(util::filename_from_url(&final_url)));
    let content_range = resp.header("content-range").map(str::to_owned);
    let content_length = resp
        .header("content-length")
        .and_then(|v| v.parse::<u64>().ok());
    let total;
    let mut ranges = false;
    if status == 206 {
        // The Content-Length of a ranged response is the length of the range
        // (usually 1), not the object. Only Content-Range carries the total.
        total = content_range.as_deref().and_then(content_range_total);
        ranges = total.is_some();
    } else {
        total = content_length;
    }
    if !(200..300).contains(&status) {
        return Err(trf!(
            "Server returned HTTP {status}.",
            "服务器返回 HTTP {status}。"
        ));
    }
    Ok(HttpProbe {
        final_url,
        name,
        total,
        ranges: ranges && total.is_some(),
    })
}

fn content_range_total(value: &str) -> Option<u64> {
    value.rsplit('/').next()?.trim().parse().ok()
}

fn content_range_start(value: &str) -> Option<u64> {
    let rest = value.strip_prefix("bytes")?.trim_start();
    let start = rest.split('-').next()?.trim();
    start.parse().ok()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum End {
    Done,
    Pause,
    Cancel,
}

fn http_part_retry(
    url: &str,
    agent: &ureq::Agent,
    start: u64,
    end: u64,
    part: &Path,
    progress: &Arc<Progress>,
    control: &Arc<Control>,
) -> Result<End, String> {
    let mut last = String::new();
    for attempt in 0..3 {
        match http_part_once(url, agent, start, end, part, progress, control) {
            Ok(e) => return Ok(e),
            Err(e) => {
                last = e;
                match control.stop() {
                    Stop::Cancel => return Ok(End::Cancel),
                    Stop::Pause => return Ok(End::Pause),
                    Stop::None => {}
                }
                if attempt < 2 {
                    std::thread::sleep(Duration::from_millis(450 * (attempt + 1) as u64));
                }
            }
        }
    }
    Err(last)
}

fn http_part_once(
    url: &str,
    agent: &ureq::Agent,
    start: u64,
    end: u64,
    part: &Path,
    progress: &Arc<Progress>,
    control: &Arc<Control>,
) -> Result<End, String> {
    match control.stop() {
        Stop::Cancel => return Ok(End::Cancel),
        Stop::Pause => return Ok(End::Pause),
        Stop::None => {}
    }
    let segment_len = end - start + 1;
    let mut existing = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    if existing > segment_len {
        let f = OpenOptions::new()
            .write(true)
            .open(part)
            .map_err(|e| trf!("Could not open partial file: {e}", "无法打开临时文件：{e}"))?;
        f.set_len(segment_len)
            .map_err(|e| trf!("Could not trim partial file: {e}", "无法截断临时文件：{e}"))?;
        existing = segment_len;
    }
    if existing == segment_len {
        return Ok(End::Done);
    }
    let request_start = start + existing;
    let resp = match agent
        .get(url)
        .set("Range", &format!("bytes={request_start}-{end}"))
        .set("Accept-Encoding", "identity")
        .call()
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => {
            return Err(trf!(
                "Server returned HTTP {code} while fetching a segment.",
                "获取分段时服务器返回 HTTP {code}。"
            ));
        }
        Err(ureq::Error::Transport(e)) => return Err(trf!("Network error: {e}", "网络错误：{e}")),
    };
    if resp.status() != 206 {
        return Err(trf!(
            "Server ignored the byte-range request (HTTP {}).",
            "服务器忽略了分段请求（HTTP {}）。",
            resp.status()
        ));
    }
    if let Some(cr) = resp.header("content-range") {
        if let Some(got_start) = content_range_start(cr) {
            if got_start != request_start {
                return Err(tr!(
                    "Server returned a segment with the wrong offset.",
                    "服务器返回的分段偏移不正确。"
                )
                .to_owned());
            }
        }
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(part)
        .map_err(|e| trf!("Could not open partial file: {e}", "无法打开临时文件：{e}"))?;
    let mut reader = resp.into_reader();
    let mut remaining = segment_len - existing;
    let mut buf = [0u8; 64 * 1024];
    while remaining > 0 {
        match control.stop() {
            Stop::Cancel => {
                let _ = file.flush();
                return Ok(End::Cancel);
            }
            Stop::Pause => {
                let _ = file.flush();
                return Ok(End::Pause);
            }
            Stop::None => {}
        }
        let want = remaining.min(buf.len() as u64) as usize;
        let n = reader
            .read(&mut buf[..want])
            .map_err(|e| trf!("Read failed: {e}", "读取失败：{e}"))?;
        if n == 0 {
            return Err(tr!(
                "The connection closed before the segment was complete.",
                "连接在分段完成前关闭。"
            )
            .to_owned());
        }
        file.write_all(&buf[..n])
            .map_err(|e| trf!("Could not write to disk: {e}", "写入磁盘失败：{e}"))?;
        progress.add_done(n as u64);
        remaining -= n as u64;
    }
    file.flush()
        .map_err(|e| trf!("Could not flush partial file: {e}", "刷新临时文件失败：{e}"))?;
    Ok(End::Done)
}

fn http_single(
    url: &str,
    agent: &ureq::Agent,
    dest: &Path,
    known_total: Option<u64>,
    id: u64,
    progress: &Arc<Progress>,
    control: &Arc<Control>,
) -> Result<RunEnd, String> {
    let part = part_path(dest, id, 0);
    let mut last = String::new();
    for attempt in 0..3 {
        match http_single_once(url, agent, &part, known_total, progress, control) {
            Ok(e) => {
                if e == End::Done {
                    if let Some(parent) = dest.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    fs::rename(&part, dest).map_err(|e| {
                        trf!("Could not finalize the download: {e}", "无法完成下载：{e}")
                    })?;
                    return Ok(RunEnd::Completed);
                }
                return Ok(if e == End::Cancel {
                    RunEnd::Canceled
                } else {
                    RunEnd::Paused
                });
            }
            Err(e) => {
                last = e;
                match control.stop() {
                    Stop::Cancel => return Ok(RunEnd::Canceled),
                    Stop::Pause => return Ok(RunEnd::Paused),
                    Stop::None => {}
                }
                if attempt < 2 {
                    std::thread::sleep(Duration::from_millis(500 * (attempt + 1) as u64));
                }
            }
        }
    }
    Err(last)
}

fn http_single_once(
    url: &str,
    agent: &ureq::Agent,
    part: &Path,
    known_total: Option<u64>,
    progress: &Arc<Progress>,
    control: &Arc<Control>,
) -> Result<End, String> {
    match control.stop() {
        Stop::Cancel => return Ok(End::Cancel),
        Stop::Pause => return Ok(End::Pause),
        Stop::None => {}
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(part)
        .map_err(|e| {
            trf!(
                "Could not create partial file: {e}",
                "无法创建临时文件：{e}"
            )
        })?;
    progress.set_done(0);
    if let Some(t) = known_total {
        progress.set_total(t);
    }
    let resp = match agent.get(url).set("Accept-Encoding", "identity").call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => {
            return Err(trf!(
                "Server returned HTTP {code}.",
                "服务器返回 HTTP {code}。"
            ));
        }
        Err(ureq::Error::Transport(e)) => return Err(trf!("Network error: {e}", "网络错误：{e}")),
    };
    if resp.status() < 200 || resp.status() >= 300 {
        return Err(trf!(
            "Server returned HTTP {}.",
            "服务器返回 HTTP {}。",
            resp.status()
        ));
    }
    let total = resp
        .header("content-length")
        .and_then(|v| v.parse::<u64>().ok())
        .or(known_total);
    if let Some(t) = total {
        if known_total.is_none() {
            progress.set_total(t);
        }
    }
    let mut reader = resp.into_reader();
    let mut buf = [0u8; 64 * 1024];
    loop {
        match control.stop() {
            Stop::Cancel => {
                let _ = file.flush();
                return Ok(End::Cancel);
            }
            Stop::Pause => {
                let _ = file.flush();
                return Ok(End::Pause);
            }
            Stop::None => {}
        }
        let n = reader
            .read(&mut buf)
            .map_err(|e| trf!("Read failed: {e}", "读取失败：{e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| trf!("Could not write to disk: {e}", "写入磁盘失败：{e}"))?;
        progress.add_done(n as u64);
    }
    file.flush()
        .map_err(|e| trf!("Could not flush partial file: {e}", "刷新临时文件失败：{e}"))?;
    let done = progress.done();
    if let Some(t) = total {
        progress.set_total(t);
        if done != t {
            return Err(trf!(
                "Download ended early ({} of {}).",
                "下载提前结束（{} / {}）。",
                format_size(done),
                format_size(t)
            ));
        }
    } else {
        progress.set_total(done);
    }
    Ok(End::Done)
}

// ---------------------------------------------------------------------- FTP

struct FtpParams {
    host: String,
    port: u16,
    user: String,
    pass: String,
    path: String,
}

fn parse_ftp(url: &str) -> Result<FtpParams, String> {
    let parsed =
        Url::parse(url).map_err(|e| trf!("Invalid FTP URL: {e}", "无效的 FTP 地址：{e}"))?;
    if parsed.scheme() != "ftp" {
        return Err(tr!("Not an FTP URL.", "不是 FTP 地址。").to_owned());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| tr!("FTP URL has no host.", "FTP 地址缺少主机名。").to_owned())?
        .to_owned();
    let port = parsed.port().unwrap_or(21);
    let user = if parsed.username().is_empty() {
        "anonymous".to_owned()
    } else {
        util::percent_decode(parsed.username())
    };
    let pass = parsed
        .password()
        .map(util::percent_decode)
        .unwrap_or_else(|| "anonymous@".to_owned());
    let mut path = util::percent_decode(parsed.path());
    if path.is_empty() {
        path.push('/');
    }
    Ok(FtpParams {
        host,
        port,
        user,
        pass,
        path,
    })
}

fn ftp_connect(p: &FtpParams) -> Result<FtpStream, String> {
    let mut ftp = FtpStream::connect((p.host.as_str(), p.port)).map_err(|e| {
        trf!(
            "Could not connect to FTP server: {e}",
            "无法连接 FTP 服务器：{e}"
        )
    })?;
    ftp.login(&p.user, &p.pass)
        .map_err(|e| trf!("FTP login failed: {e}", "FTP 登录失败：{e}"))?;
    ftp.transfer_type(FileType::Binary).map_err(|e| {
        trf!(
            "Could not switch to binary mode: {e}",
            "无法切换到二进制模式：{e}"
        )
    })?;
    Ok(ftp)
}

fn ftp_size(p: &FtpParams) -> Result<Option<u64>, String> {
    let mut ftp = ftp_connect(p)?;
    let size = ftp.size(&p.path).ok().map(|v| v as u64);
    let _ = ftp.quit();
    Ok(size)
}

fn run_ftp(
    job: &Job,
    store: &TaskStore,
    control: &Arc<Control>,
    progress: &Arc<Progress>,
) -> Result<RunEnd, String> {
    if let Stop::Cancel | Stop::Pause = control.stop() {
        return Ok(RunEnd::from_stop(control.stop()));
    }
    let params = parse_ftp(&job.url)?;
    let total = ftp_size(&params)?;
    if let Some(t) = total {
        progress.set_total(t);
    }
    let name = job.name.clone();
    let (dest, was_existing) = match job.dest_path.clone() {
        Some(p) => (p, true),
        None => (
            reserve_destination(store, job.id, &job.dest_dir, &name),
            false,
        ),
    };
    update_if_current(store, job.id, control, |t| {
        if !was_existing {
            t.dest_path = Some(dest.clone());
        }
    });
    let dest = if !was_existing && dest.exists() {
        reserve_destination(store, job.id, &job.dest_dir, &name)
    } else {
        dest
    };
    let params = Arc::new(params);
    if let Some(total) = total {
        if total == 0 {
            let _ = File::create(&dest)
                .map_err(|e| trf!("Could not create file: {e}", "无法创建文件：{e}"))?;
            return Ok(RunEnd::Completed);
        }
        let segments = split_ranges(total, job.connections);
        if segments.len() > 1 {
            let parts: Vec<PathBuf> = segments
                .iter()
                .enumerate()
                .map(|(i, _)| part_path(&dest, job.id, i))
                .collect();
            prepare_parts(&parts, &segments, progress)?;
            let mut handles = Vec::new();
            for (i, (start, end)) in segments.iter().copied().enumerate() {
                let params = params.clone();
                let part = parts[i].clone();
                let progress = progress.clone();
                let control = control.clone();
                handles.push(std::thread::spawn(move || {
                    ftp_part_retry(&params, start, end, &part, &progress, &control)
                }));
            }
            let mut pause = false;
            let mut cancel = false;
            let mut error: Option<String> = None;
            for h in handles {
                match h.join() {
                    Ok(Ok(End::Done)) => {}
                    Ok(Ok(End::Pause)) => pause = true,
                    Ok(Ok(End::Cancel)) => cancel = true,
                    Ok(Err(e)) => {
                        if error.is_none() {
                            error = Some(e);
                        }
                    }
                    Err(_) => {
                        if error.is_none() {
                            error = Some(
                                tr!("An FTP worker panicked.", "FTP 线程发生错误。").to_owned(),
                            );
                        }
                    }
                }
            }
            if cancel {
                return Ok(RunEnd::Canceled);
            }
            if pause {
                return Ok(RunEnd::Paused);
            }
            if let Some(e) = error {
                return Err(e);
            }
            merge_parts(&dest, &parts, job.id)?;
            update_if_current(store, job.id, control, |t| {
                t.dest_path = Some(dest.clone());
            });
            return Ok(RunEnd::Completed);
        }
    }
    ftp_single(&params, &dest, total, job.id, progress, control)
}

fn ftp_part_retry(
    params: &FtpParams,
    start: u64,
    end: u64,
    part: &Path,
    progress: &Arc<Progress>,
    control: &Arc<Control>,
) -> Result<End, String> {
    let mut last = String::new();
    for attempt in 0..3 {
        match ftp_part_once(params, start, end, part, progress, control) {
            Ok(e) => return Ok(e),
            Err(e) => {
                last = e;
                match control.stop() {
                    Stop::Cancel => return Ok(End::Cancel),
                    Stop::Pause => return Ok(End::Pause),
                    Stop::None => {}
                }
                if attempt < 2 {
                    std::thread::sleep(Duration::from_millis(500 * (attempt + 1) as u64));
                }
            }
        }
    }
    Err(last)
}

fn ftp_part_once(
    params: &FtpParams,
    start: u64,
    end: u64,
    part: &Path,
    progress: &Arc<Progress>,
    control: &Arc<Control>,
) -> Result<End, String> {
    match control.stop() {
        Stop::Cancel => return Ok(End::Cancel),
        Stop::Pause => return Ok(End::Pause),
        Stop::None => {}
    }
    let segment_len = end - start + 1;
    let mut existing = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    if existing > segment_len {
        File::create(part)
            .map_err(|e| trf!("Could not trim partial file: {e}", "无法截断临时文件：{e}"))?;
        existing = 0;
    }
    if existing == segment_len {
        return Ok(End::Done);
    }
    let mut ftp = ftp_connect(params)?;
    if existing > 0 {
        ftp.resume_transfer((start + existing) as usize)
            .map_err(|e| trf!("FTP REST failed: {e}", "FTP 续传请求失败：{e}"))?;
    } else if start > 0 {
        ftp.resume_transfer(start as usize)
            .map_err(|e| trf!("FTP REST failed: {e}", "FTP 续传请求失败：{e}"))?;
    }
    let mut stream = ftp
        .retr_as_stream(&params.path)
        .map_err(|e| trf!("FTP RETR failed: {e}", "FTP 下载请求失败：{e}"))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(part)
        .map_err(|e| trf!("Could not open partial file: {e}", "无法打开临时文件：{e}"))?;
    let mut remaining = segment_len - existing;
    let mut buf = [0u8; 64 * 1024];
    while remaining > 0 {
        match control.stop() {
            Stop::Cancel => {
                let _ = file.flush();
                let _ = ftp.abort(stream);
                return Ok(End::Cancel);
            }
            Stop::Pause => {
                let _ = file.flush();
                let _ = ftp.abort(stream);
                return Ok(End::Pause);
            }
            Stop::None => {}
        }
        let want = remaining.min(buf.len() as u64) as usize;
        let n = stream
            .read(&mut buf[..want])
            .map_err(|e| trf!("FTP read failed: {e}", "FTP 读取失败：{e}"))?;
        if n == 0 {
            return Err(tr!(
                "FTP server closed the data connection early.",
                "FTP 服务器提前关闭了数据连接。"
            )
            .to_owned());
        }
        file.write_all(&buf[..n])
            .map_err(|e| trf!("Could not write to disk: {e}", "写入磁盘失败：{e}"))?;
        progress.add_done(n as u64);
        remaining -= n as u64;
    }
    file.flush()
        .map_err(|e| trf!("Could not flush partial file: {e}", "刷新临时文件失败：{e}"))?;
    let _ = stream.finish();
    let _ = ftp.quit();
    Ok(End::Done)
}

fn ftp_single(
    params: &FtpParams,
    dest: &Path,
    total: Option<u64>,
    id: u64,
    progress: &Arc<Progress>,
    control: &Arc<Control>,
) -> Result<RunEnd, String> {
    let part = part_path(dest, id, 0);
    let mut last = String::new();
    for attempt in 0..3 {
        match ftp_single_once(params, &part, total, progress, control) {
            Ok(e) => {
                if e == End::Done {
                    if let Some(parent) = dest.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    fs::rename(&part, dest).map_err(|e| {
                        trf!("Could not finalize the download: {e}", "无法完成下载：{e}")
                    })?;
                    return Ok(RunEnd::Completed);
                }
                return Ok(if e == End::Cancel {
                    RunEnd::Canceled
                } else {
                    RunEnd::Paused
                });
            }
            Err(e) => {
                last = e;
                match control.stop() {
                    Stop::Cancel => return Ok(RunEnd::Canceled),
                    Stop::Pause => return Ok(RunEnd::Paused),
                    Stop::None => {}
                }
                if attempt < 2 {
                    std::thread::sleep(Duration::from_millis(500 * (attempt + 1) as u64));
                }
            }
        }
    }
    Err(last)
}

fn ftp_single_once(
    params: &FtpParams,
    part: &Path,
    total: Option<u64>,
    progress: &Arc<Progress>,
    control: &Arc<Control>,
) -> Result<End, String> {
    match control.stop() {
        Stop::Cancel => return Ok(End::Cancel),
        Stop::Pause => return Ok(End::Pause),
        Stop::None => {}
    }
    // A single-stream transfer only resumes when the size is known, which
    // implies the server supports SIZE/REST in the common case.
    let mut existing = if total.is_some() {
        fs::metadata(part).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };
    if let Some(t) = total {
        if existing > t {
            File::create(part)
                .map_err(|e| trf!("Could not reset partial file: {e}", "无法重置临时文件：{e}"))?;
            existing = 0;
        }
        progress.set_total(t);
        progress.set_done(existing);
    } else {
        progress.set_done(0);
    }
    let mut ftp = ftp_connect(params)?;
    if existing > 0 {
        ftp.resume_transfer(existing as usize)
            .map_err(|e| trf!("FTP REST failed: {e}", "FTP 续传请求失败：{e}"))?;
    }
    let mut stream = ftp
        .retr_as_stream(&params.path)
        .map_err(|e| trf!("FTP RETR failed: {e}", "FTP 下载请求失败：{e}"))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(part)
        .map_err(|e| trf!("Could not open partial file: {e}", "无法打开临时文件：{e}"))?;
    let mut buf = [0u8; 64 * 1024];
    loop {
        match control.stop() {
            Stop::Cancel => {
                let _ = file.flush();
                let _ = ftp.abort(stream);
                return Ok(End::Cancel);
            }
            Stop::Pause => {
                let _ = file.flush();
                let _ = ftp.abort(stream);
                return Ok(End::Pause);
            }
            Stop::None => {}
        }
        let n = stream
            .read(&mut buf)
            .map_err(|e| trf!("FTP read failed: {e}", "FTP 读取失败：{e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| trf!("Could not write to disk: {e}", "写入磁盘失败：{e}"))?;
        progress.add_done(n as u64);
    }
    file.flush()
        .map_err(|e| trf!("Could not flush partial file: {e}", "刷新临时文件失败：{e}"))?;
    let _ = stream.finish();
    let _ = ftp.quit();
    if let Some(t) = total {
        if progress.done() != t {
            return Err(trf!(
                "FTP transfer ended early ({} of {}).",
                "FTP 传输提前结束（{} / {}）。",
                format_size(progress.done()),
                format_size(t)
            ));
        }
    } else {
        progress.set_total(progress.done());
    }
    Ok(End::Done)
}

// ------------------------------------------------------------------ files

fn split_ranges(total: u64, connections: usize) -> Vec<(u64, u64)> {
    let max_segments = connections.clamp(1, 32) as u64;
    let by_size = (total / MIN_SEGMENT_BYTES).max(1);
    let n = max_segments.min(by_size).max(1);
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let start = total * i / n;
        let end = total * (i + 1) / n - 1;
        out.push((start, end));
    }
    out
}

fn part_path(dest: &Path, id: u64, index: usize) -> PathBuf {
    let filename = dest
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_owned());
    dest.with_file_name(format!(".{filename}.flowdown-{id}-{index}.part"))
}

fn merge_temp_path(dest: &Path, id: u64) -> PathBuf {
    let filename = dest
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_owned());
    dest.with_file_name(format!(".{filename}.flowdown-{id}.merge"))
}

fn prepare_parts(
    parts: &[PathBuf],
    segments: &[(u64, u64)],
    progress: &Arc<Progress>,
) -> Result<(), String> {
    let mut done = 0u64;
    for (part, (start, end)) in parts.iter().zip(segments.iter().copied()) {
        let len = end - start + 1;
        let mut existing = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
        if existing > len {
            let f = OpenOptions::new()
                .write(true)
                .open(part)
                .map_err(|e| trf!("Could not open partial file: {e}", "无法打开临时文件：{e}"))?;
            f.set_len(len)
                .map_err(|e| trf!("Could not trim partial file: {e}", "无法截断临时文件：{e}"))?;
            existing = len;
        }
        done += existing;
    }
    progress.set_done(done);
    Ok(())
}

fn reserve_destination(store: &TaskStore, id: u64, dir: &Path, name: &str) -> PathBuf {
    let _ = fs::create_dir_all(dir);
    let mut tasks = match store.lock() {
        Ok(t) => t,
        Err(_) => return dir.join(util::sanitize_filename(name)),
    };
    let mut reserved = HashSet::new();
    for t in tasks.iter() {
        if t.id != id {
            if let Some(p) = &t.dest_path {
                reserved.insert(p.clone());
            }
        }
    }
    let path = unique_path(dir, name, &reserved);
    if let Some(t) = tasks.iter_mut().find(|t| t.id == id) {
        t.dest_path = Some(path.clone());
        t.name = util::sanitize_filename(name);
    }
    path
}

fn merge_parts(dest: &Path, parts: &[PathBuf], id: u64) -> Result<(), String> {
    let merge = merge_temp_path(dest, id);
    if let Some(parent) = dest.parent() {
        let _ = fs::create_dir_all(parent);
    }
    {
        let mut out = File::create(&merge)
            .map_err(|e| trf!("Could not create merge file: {e}", "无法创建合并文件：{e}"))?;
        for part in parts {
            let mut input = File::open(part)
                .map_err(|e| trf!("Could not read partial file: {e}", "无法读取临时文件：{e}"))?;
            std::io::copy(&mut input, &mut out)
                .map_err(|e| trf!("Could not merge download: {e}", "无法合并下载文件：{e}"))?;
        }
        out.flush()
            .map_err(|e| trf!("Could not flush merged file: {e}", "无法刷新合并文件：{e}"))?;
    }
    fs::rename(&merge, dest).map_err(|e| {
        trf!(
            "Could not move completed file: {e}",
            "无法移动已完成的文件：{e}"
        )
    })?;
    for part in parts {
        let _ = fs::remove_file(part);
    }
    Ok(())
}

fn cleanup_parts(dest: &Path, id: u64, count: usize) -> std::io::Result<()> {
    for i in 0..count.max(1) {
        let _ = fs::remove_file(part_path(dest, id, i));
    }
    let _ = fs::remove_file(merge_temp_path(dest, id));
    Ok(())
}

/// A worker may learn the final destination after starting (Content-Disposition
/// or a torrent name), so clean-up must consult the live task row first.
fn cleanup_task_parts(store: &TaskStore, job: &Job, control: &Arc<Control>) {
    let mut dest = None;
    let mut connections = job.connections;
    if let Ok(tasks) = store.lock() {
        if let Some(task) = tasks.iter().find(|t| t.id == job.id) {
            if Arc::ptr_eq(&task.control, control) {
                dest = task.dest_path.clone();
                connections = task.connections;
            }
        }
    }
    if dest.is_none() {
        dest = job.dest_path.clone();
    }
    if let Some(d) = dest {
        let _ = cleanup_parts(&d, job.id, connections.max(64));
    }
}

// ------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_cover_the_file() {
        let r = split_ranges(1024 * 1024 * 10, 8);
        assert_eq!(r.first().unwrap().0, 0);
        assert_eq!(r.last().unwrap().1, 1024 * 1024 * 10 - 1);
        for w in r.windows(2) {
            assert_eq!(w[0].1 + 1, w[1].0);
        }
    }

    #[test]
    fn small_files_use_one_segment() {
        assert_eq!(split_ranges(1000, 8).len(), 1);
    }

    #[test]
    fn content_range_parsing() {
        assert_eq!(content_range_total("bytes 0-0/12345"), Some(12345));
        assert_eq!(content_range_start("bytes 500-999/1000"), Some(500));
    }
}
