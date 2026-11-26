//! File storage that does not keep downloaded files locked.
//!
//! librqbit's own `FilesystemStorage` opens every file read/write for as long
//! as the torrent is in the session. Windows refuses to start an executable
//! while any handle has it open for writing, so a downloaded installer could
//! not be run until the client was closed.
//!
//! This storage opens read-only handles for hashing and seeding, opens a write
//! handle only when data actually arrives, and closes write handles after a few
//! seconds without writes. A finished download therefore only has read handles
//! open (shared for read, write and delete), which do not block running,
//! moving or deleting the files.

use std::{
    any::TypeId,
    collections::HashMap,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, RwLock, Weak},
    time::{Duration, Instant},
};

use anyhow::Context;
use librqbit::{
    storage::{
        filesystem::{FilesystemStorageFactory, OurFileExt},
        BoxStorageFactory, StorageFactory, StorageFactoryExt, TorrentStorage,
    },
    ManagedTorrentShared, TorrentMetadata,
};

/// How long a write handle may stay idle before it is closed.
const WRITE_IDLE: Duration = Duration::from_secs(4);

/// Output folder of every torrent, by lowercase hex info hash. librqbit does not
/// expose a torrent's output folder to storage factories, so the app records it
/// here before adding a torrent (and from the persisted session on startup).
fn folders() -> &'static Mutex<HashMap<String, PathBuf>> {
    static FOLDERS: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    FOLDERS.get_or_init(Default::default)
}

pub fn register_output_folder(info_hash_hex: &str, folder: PathBuf) {
    folders().lock().unwrap().insert(info_hash_hex.to_ascii_lowercase(), folder);
}

/// Reads librqbit's `session.json` and registers the folder of every saved torrent.
pub fn register_persisted_folders(session_json: &Path) {
    let Ok(bytes) = std::fs::read(session_json) else { return };
    let Ok(db) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return };
    let Some(torrents) = db.get("torrents").and_then(|t| t.as_object()) else { return };
    for torrent in torrents.values() {
        if let (Some(hash), Some(folder)) = (
            torrent.get("info_hash").and_then(|v| v.as_str()),
            torrent.get("output_folder").and_then(|v| v.as_str()),
        ) {
            register_output_folder(hash, PathBuf::from(folder));
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct GentleStorageFactory;

impl StorageFactory for GentleStorageFactory {
    type Storage = Box<dyn TorrentStorage>;

    fn create(&self, shared: &ManagedTorrentShared, metadata: &TorrentMetadata) -> anyhow::Result<Self::Storage> {
        let hash = shared.info_hash.as_string().to_ascii_lowercase();
        match folders().lock().unwrap().get(&hash).cloned() {
            Some(folder) => Ok(Box::new(GentleStorage { output_folder: folder, files: RwLock::new(None) })),
            // Unknown folder: behave exactly like the stock storage.
            None => Ok(Box::new(FilesystemStorageFactory::default().create(shared, metadata)?)),
        }
    }

    /// librqbit only persists torrents whose storage is the filesystem one.
    /// This storage writes to the same files in the same layout, so the
    /// persisted session (and fast resume) stay valid.
    fn is_type_id(&self, type_id: TypeId) -> bool {
        type_id == TypeId::of::<FilesystemStorageFactory>() || type_id == TypeId::of::<Self>()
    }

    fn clone_box(&self) -> BoxStorageFactory {
        self.boxed()
    }
}

struct Slot {
    path: PathBuf,
    padding: bool,
    reader: RwLock<Option<Arc<File>>>,
    writer: RwLock<Option<Arc<File>>>,
    last_write: Mutex<Instant>,
    #[cfg(windows)]
    sparse_tried: std::sync::atomic::AtomicBool,
}

impl Slot {
    fn reader(&self) -> anyhow::Result<Arc<File>> {
        // Prefer the write handle while one is open so reads see fresh writes
        // through the same handle; otherwise use (or open) a read-only one.
        if let Some(writer) = self.writer.read().unwrap().clone() { return Ok(writer); }
        if let Some(reader) = self.reader.read().unwrap().clone() { return Ok(reader); }
        let mut guard = self.reader.write().unwrap();
        if let Some(reader) = guard.clone() { return Ok(reader); }
        let file = Arc::new(File::open(&self.path).with_context(|| format!("error opening {:?} for reading", self.path))?);
        *guard = Some(file.clone());
        Ok(file)
    }

    fn writer(&self) -> anyhow::Result<Arc<File>> {
        *self.last_write.lock().unwrap() = Instant::now();
        if let Some(writer) = self.writer.read().unwrap().clone() { return Ok(writer); }
        let mut guard = self.writer.write().unwrap();
        if let Some(writer) = guard.clone() { return Ok(writer); }
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&self.path)
            .with_context(|| format!("error opening {:?} in read/write mode", self.path))?;
        #[cfg(windows)]
        if !self.sparse_tried.swap(true, std::sync::atomic::Ordering::Relaxed) { mark_sparse(&file); }
        let file = Arc::new(file);
        *guard = Some(file.clone());
        Ok(file)
    }

    fn close_idle_writer(&self, now: Instant) {
        let idle = now.duration_since(*self.last_write.lock().unwrap()) >= WRITE_IDLE;
        if idle && self.writer.read().unwrap().is_some() { *self.writer.write().unwrap() = None; }
    }

    fn close_all(&self) {
        *self.writer.write().unwrap() = None;
        *self.reader.write().unwrap() = None;
    }
}

type Slots = Arc<Vec<Slot>>;

pub struct GentleStorage {
    output_folder: PathBuf,
    /// `None` before `init` and after `take`.
    files: RwLock<Option<Slots>>,
}

impl GentleStorage {
    fn slot<T>(&self, file_id: usize, f: impl FnOnce(&Slot) -> anyhow::Result<T>) -> anyhow::Result<T> {
        let files = self.files.read().unwrap().clone().context("storage is not initialized")?;
        let slot = files.get(file_id).context("no such file")?;
        if slot.padding { anyhow::bail!("padding files have no storage"); }
        f(slot)
    }
}

impl TorrentStorage for GentleStorage {
    fn init(&mut self, shared: &ManagedTorrentShared, metadata: &TorrentMetadata) -> anyhow::Result<()> {
        let _ = shared;
        let mut slots = Vec::with_capacity(metadata.file_infos.len());
        for info in metadata.file_infos.iter() {
            let path = self.output_folder.join(&info.relative_filename);
            if !info.attrs.padding {
                std::fs::create_dir_all(path.parent().context("bug: no parent")?)?;
                // Create missing files like the stock storage does, then let the handle go.
                OpenOptions::new().write(true).create(true).truncate(false).open(&path)
                    .with_context(|| format!("error opening {path:?} in read/write mode"))?;
            }
            slots.push(Slot {
                path,
                padding: info.attrs.padding,
                reader: RwLock::new(None),
                writer: RwLock::new(None),
                last_write: Mutex::new(Instant::now()),
                #[cfg(windows)]
                sparse_tried: Default::default(),
            });
        }
        let slots = Arc::new(slots);
        sweeper().lock().unwrap().push(Arc::downgrade(&slots));
        *self.files.write().unwrap() = Some(slots);
        Ok(())
    }

    fn pread_exact(&self, file_id: usize, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        self.slot(file_id, |slot| slot.reader()?.pread_exact(offset, buf))
    }

    fn pwrite_all(&self, file_id: usize, offset: u64, buf: &[u8]) -> anyhow::Result<()> {
        self.slot(file_id, |slot| slot.writer()?.pwrite_all(offset, buf))
    }

    fn remove_file(&self, file_id: usize, filename: &Path) -> anyhow::Result<()> {
        if let Some(files) = self.files.read().unwrap().as_ref() {
            if let Some(slot) = files.get(file_id) { slot.close_all(); }
        }
        Ok(std::fs::remove_file(self.output_folder.join(filename))?)
    }

    fn remove_directory_if_empty(&self, path: &Path) -> anyhow::Result<()> {
        let path = self.output_folder.join(path);
        if !path.is_dir() { anyhow::bail!("cannot remove dir: {path:?} is not a directory"); }
        if std::fs::read_dir(&path)?.next().is_none() {
            std::fs::remove_dir(&path).with_context(|| format!("error removing {path:?}"))?;
        }
        Ok(())
    }

    fn ensure_file_length(&self, file_id: usize, length: u64) -> anyhow::Result<()> {
        self.slot(file_id, |slot| {
            // Finished files already have the right size: do not open them for writing.
            if std::fs::metadata(&slot.path).map(|m| m.len()).ok() == Some(length) { return Ok(()); }
            Ok(slot.writer()?.set_len(length)?)
        })
    }

    fn take(&self) -> anyhow::Result<Box<dyn TorrentStorage>> {
        let files = self.files.write().unwrap().take();
        Ok(Box::new(GentleStorage { output_folder: self.output_folder.clone(), files: RwLock::new(files) }))
    }
}

impl Drop for GentleStorage {
    fn drop(&mut self) {
        // Close handles right away when a torrent is paused or removed, instead
        // of waiting for the last Arc clone held by an in-flight read.
        if let Some(files) = self.files.get_mut().unwrap().take() {
            for slot in files.iter() { slot.close_all(); }
        }
    }
}

/// Every live storage, checked periodically by one background thread that
/// closes idle write handles.
fn sweeper() -> &'static Mutex<Vec<Weak<Vec<Slot>>>> {
    static SWEEPER: OnceLock<Mutex<Vec<Weak<Vec<Slot>>>>> = OnceLock::new();
    SWEEPER.get_or_init(|| {
        std::thread::Builder::new().name("idle-file-closer".into()).spawn(|| loop {
            std::thread::sleep(Duration::from_secs(1));
            let live: Vec<_> = {
                let mut list = sweeper().lock().unwrap();
                list.retain(|weak| weak.strong_count() > 0);
                list.iter().filter_map(Weak::upgrade).collect()
            };
            let now = Instant::now();
            for slots in live { for slot in slots.iter() { slot.close_idle_writer(now); } }
        }).expect("failed to start idle file closer");
        Mutex::new(Vec::new())
    })
}

/// Marks a file sparse so growing it does not make NTFS zero-fill the gap
/// (librqbit's own storage does the same).
#[cfg(windows)]
fn mark_sparse(file: &File) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::{Ioctl::FSCTL_SET_SPARSE, IO::DeviceIoControl};
    let mut returned = 0u32;
    // Best effort: on filesystems without sparse support the call just fails.
    unsafe {
        DeviceIoControl(file.as_raw_handle() as _, FSCTL_SET_SPARSE, std::ptr::null(), 0, std::ptr::null_mut(), 0, &mut returned, std::ptr::null_mut());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(path: PathBuf) -> Slot {
        Slot { path, padding: false, reader: RwLock::new(None), writer: RwLock::new(None), last_write: Mutex::new(Instant::now()), #[cfg(windows)] sparse_tried: Default::default() }
    }

    #[test]
    fn finished_file_is_not_held_for_writing() {
        let dir = std::env::temp_dir().join(format!("critter-storage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("file.bin");
        let slot = slot(path.clone());
        slot.writer().unwrap().pwrite_all(0, b"meow").unwrap();
        // Opening for write while sharing only reads is what running an exe
        // needs. It must fail while the client is writing...
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            assert!(OpenOptions::new().write(true).share_mode(1 /* FILE_SHARE_READ */).open(&path).is_err());
        }
        slot.close_idle_writer(Instant::now() + WRITE_IDLE);
        assert!(slot.writer.read().unwrap().is_none(), "idle writer must be closed");
        let mut buf = [0u8; 4];
        slot.reader().unwrap().pread_exact(0, &mut buf).unwrap();
        assert_eq!(&buf, b"meow");
        // ...and succeed once only the read handle is left.
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            OpenOptions::new().write(true).share_mode(1 /* FILE_SHARE_READ */).open(&path)
                .expect("file must not be locked by the client");
        }
        slot.close_all();
        std::fs::remove_dir_all(&dir).ok();
    }
}
