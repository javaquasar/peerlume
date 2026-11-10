//! Owner-issued, single-use confirmations bound to filesystem identities.
use super::*;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::Path, time::Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Context {
    root: PathBuf,
    source: PathBuf,
    total: usize,
}
pub(super) struct PreparedTrash {
    client: String,
    until: Instant,
    plan: TrashPlan,
    context: Context,
}
struct Targets {
    paths: Vec<PathBuf>,
    digest: String,
}
struct Busy {
    owner: Arc<Mutex<Controller>>,
    id: String,
}
impl Drop for Busy {
    fn drop(&mut self) {
        let mut owner = self.owner.lock().unwrap_or_else(|error| error.into_inner());
        owner.frontend_busy.remove(&self.id);
        owner.owner_wake().notify_one();
    }
}

impl Service {
    fn context(&self, owner: &mut Controller, id: &str) -> Result<Context, Error> {
        let index = owner.torrent_index(id).ok_or(Error::Stale)?;
        let item = &owner.torrents[index];
        let effective = crate::torrent::EffectiveTorrentSettings::resolve(
            &self
                .settings
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
            &item.overrides,
            item.is_private,
        );
        Ok(Context {
            root: effective.download_directory.into(),
            source: item.source.clone().into(),
            total: item.files.file_count(),
        })
    }

    pub(super) fn prepare_trash(
        &self,
        client: String,
        id: String,
        revision: u64,
        scope: TrashScope,
    ) -> Result<Reply, Error> {
        let context = {
            let mut owner = self.owner.lock().unwrap_or_else(|error| error.into_inner());
            check_revision(&owner, &id, revision)?;
            self.context(&mut owner, &id)?
        };
        let targets = self.targets(&id, scope, &context)?;
        {
            let mut owner = self.owner.lock().unwrap_or_else(|error| error.into_inner());
            check_revision(&owner, &id, revision)?;
            if self.context(&mut owner, &id)? != context {
                return Err(Error::Stale);
            }
        }
        let plan = TrashPlan {
            id,
            plan_id: crate::control::access::random_id().map_err(|_| Error::Busy)?,
            digest: targets.digest,
            scope,
            root: context.root.to_string_lossy().into_owned(),
            source: context.source.to_string_lossy().into_owned(),
            count: targets.paths.len(),
            preview: targets
                .paths
                .iter()
                .take(8)
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            revision,
        };
        // Plans retain only the digest/context, never an unbounded path set.
        let mut plans = self.plans.lock().unwrap_or_else(|error| error.into_inner());
        plans.retain(|_, plan| plan.until > Instant::now() && plan.client != client);
        if plans.len() >= 8 {
            return Err(Error::Busy);
        }
        plans.insert(
            plan.plan_id.clone(),
            PreparedTrash {
                client,
                until: Instant::now() + Duration::from_secs(300),
                plan: plan.clone(),
                context,
            },
        );
        Ok(Reply::TrashPlan(plan))
    }

    pub(super) fn commit_trash(
        self: &Arc<Self>,
        client: String,
        plan_id: String,
        digest: String,
    ) -> Result<Reply, Error> {
        let prepared = {
            let mut plans = self.plans.lock().unwrap_or_else(|error| error.into_inner());
            plans.retain(|_, plan| plan.until > Instant::now());
            let plan = plans.get(&plan_id).ok_or(Error::Stale)?;
            if plan.client != client || plan.plan.digest != digest {
                return Err(Error::Stale);
            }
            plans.remove(&plan_id).ok_or(Error::Stale)?
        };
        let id = prepared.plan.id.clone();
        {
            let mut owner = self.owner.lock().unwrap_or_else(|error| error.into_inner());
            check_revision(&owner, &id, prepared.plan.revision)?;
            if self.context(&mut owner, &id)? != prepared.context {
                return Err(Error::Stale);
            }
            owner.frontend_busy.insert(id.clone());
        }
        let _busy = Busy {
            owner: self.owner.clone(),
            id: id.clone(),
        };
        let result = self.execute_trash(&prepared);
        if let Err(error) = &result {
            let mut owner = self.owner.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(index) = owner.torrent_index(&id) {
                owner.torrents[index]
                    .runtime
                    .record_error(&error.to_string());
                if !owner.torrents[index].runtime.registered {
                    owner.torrents[index].state = TorrentState::Error;
                }
            }
            owner.record_native_notice(&crate::client::SessionNotice::Error {
                id: id.clone(),
                message: error.to_string(),
            });
            owner.owner_wake().notify_one();
        }
        result
    }

    fn execute_trash(&self, prepared: &PreparedTrash) -> Result<Reply, Error> {
        let plan = &prepared.plan;
        let targets = self.targets(&plan.id, plan.scope, &prepared.context)?;
        if targets.digest != plan.digest {
            return Err(Error::Failed("Confirmed file locations or identities changed; no files were moved. Confirm again".into()));
        }
        let before = {
            let mut owner = self.owner.lock().unwrap_or_else(|error| error.into_inner());
            if self.context(&mut owner, &plan.id)? != prepared.context {
                return Err(Error::Stale);
            }
            let before = owner.detachment_generation(&plan.id);
            // Reservation stays atomic with admission, while the ordinary
            // controller validates revisions and supplies the command receipt.
            owner.frontend_busy.remove(&plan.id);
            let accepted = owner.submit(
                &plan.id,
                crate::control::TorrentCommand::Remove,
                Some(plan.revision),
            );
            owner.frontend_busy.insert(plan.id.clone());
            accepted.map_err(Error::Command)?;
            owner.owner_wake().notify_one();
            before
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if self
                .owner
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .detachment_generation(&plan.id)
                != before
            {
                break;
            }
            if self.stop.load(Ordering::Acquire) || Instant::now() >= deadline {
                return Err(Error::Failed(
                    "Fresh engine detachment was not acknowledged; no files were moved".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        {
            let mut owner = self.owner.lock().unwrap_or_else(|error| error.into_inner());
            let index = owner.torrent_index(&plan.id).ok_or(Error::Stale)?;
            let item = &mut owner.torrents[index];
            item.runtime.registered = false;
            item.state = TorrentState::Paused;
            item.download_rate = 0;
            item.upload_rate = 0;
            item.peers = 0;
            item.seeds = 0;
            item.completion = Default::default();
            let scalar = crate::state::ScalarUpdate::from_torrent(item);
            owner
                .backend
                .persist_native_stopped(&scalar)
                .map_err(Error::Failed)?;
            if self.context(&mut owner, &plan.id)? != prepared.context {
                return Err(Error::Stale);
            }
        }
        // Recheck after detach and the durable stopped intent, immediately
        // before the only destructive callback. Growing the same file is fine;
        // replacing it, moving storage, adding or removing a path is not.
        let current = self.targets(&plan.id, plan.scope, &prepared.context)?;
        if current.digest != plan.digest {
            return Err(Error::Failed("Confirmed file locations or identities changed; stopped torrent retained. Confirm again".into()));
        }
        if !current.paths.is_empty() {
            trash::delete_all(&current.paths).map_err(|error| {
                Error::Failed(format!(
                    "Some files may already have moved; stopped torrent retained: {error}"
                ))
            })?;
        }
        let mut owner = self.owner.lock().unwrap_or_else(|error| error.into_inner());
        owner.backend.remove_torrent(&plan.id).map_err(|error| Error::Failed(format!("Files were recycled but durable row removal failed; stopped torrent retained: {error}")))?;
        owner
            .torrents
            .retain(|item| item.info_hashes.join(";") != plan.id);
        owner.invalidate_torrent_index();
        owner.publish();
        owner.owner_wake().notify_one();
        Ok(Reply::Done(
            "Confirmed files recycled and torrent removed".into(),
        ))
    }

    fn targets(&self, id: &str, scope: TrashScope, context: &Context) -> Result<Targets, Error> {
        let mut paths = BTreeSet::new();
        let canonical_root =
            if matches!(scope, TrashScope::DownloadedFiles | TrashScope::Everything) {
                match context.root.canonicalize() {
                    Ok(root) => Some(root),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(error) => {
                        return Err(Error::Failed(format!(
                            "Cannot inspect download directory; no files were moved: {error}"
                        )));
                    }
                }
            } else {
                None
            };
        if let Some(root) = &canonical_root {
            let mut offset = 0;
            let mut bytes = 0usize;
            while offset < context.total {
                if self.stop.load(Ordering::Acquire) {
                    return Err(Error::Busy);
                }
                let page = self
                    .owner
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .backend
                    .load_file_page(id, offset, 128)
                    .map_err(Error::Failed)?;
                if page.total != context.total {
                    return Err(Error::Stale);
                }
                let end = offset
                    .checked_add(page.files.len())
                    .ok_or(Error::TooLarge)?;
                for file in page.files {
                    bytes = bytes.saturating_add(file.path.len());
                    if bytes > 64 * 1024 * 1024 {
                        return Err(Error::TooLarge);
                    }
                    crate::limits::validate_relative_paths(
                        [file.path.as_str()],
                        &crate::limits::ResourceLimits::default(),
                    )
                    .map_err(Error::Failed)?;
                    let relative: PathBuf = file.path.split(['/', '\\']).collect();
                    let candidate = context.root.join(relative);
                    if let Some(canonical) = existing(&candidate)? {
                        if !canonical.starts_with(root) {
                            return Err(Error::Failed("Torrent path leaves the confirmed download directory; no files were moved".into()));
                        }
                        paths.insert(candidate);
                    }
                }
                match page.next_offset {
                    Some(next) if next == end && next > offset => offset = next,
                    Some(_) => return Err(Error::Stale),
                    None => {
                        if end != context.total {
                            return Err(Error::Stale);
                        }
                        break;
                    }
                }
            }
        }
        if matches!(scope, TrashScope::TorrentFile | TrashScope::Everything)
            && context
                .source
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("torrent"))
            && existing(&context.source)?.is_some()
        {
            paths.insert(context.source.clone());
        }
        let mut hash = Sha256::new();
        hash.update(id.as_bytes());
        hash.update([scope as u8]);
        hash_path(&mut hash, &context.root);
        hash_path(&mut hash, &context.source);
        if let Some(root) = canonical_root {
            hash_path(&mut hash, &root);
        }
        hash.update((context.total as u64).to_le_bytes());
        for path in &paths {
            let canonical = existing(path)?.ok_or(Error::Stale)?;
            hash_path(&mut hash, path);
            hash_path(&mut hash, &canonical);
            hash.update(file_identity(path)?);
        }
        Ok(Targets {
            paths: paths.into_iter().collect(),
            digest: format!("{:x}", hash.finalize()),
        })
    }
}

fn existing(path: &Path) -> Result<Option<PathBuf>, Error> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => path
            .canonicalize()
            .map(Some)
            .map_err(|error| Error::Failed(error.to_string())),
        Ok(_) => Err(Error::Failed(
            "A confirmed torrent path is not a regular file; no files were moved".into(),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::Failed(format!(
            "Cannot inspect confirmed file; no files were moved: {error}"
        ))),
    }
}
fn hash_path(hash: &mut Sha256, path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let bytes = path.as_os_str().as_bytes();
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<_> = path.as_os_str().encode_wide().collect();
        hash.update((wide.len() as u64).to_le_bytes());
        for unit in wide {
            hash.update(unit.to_le_bytes());
        }
    }
}
fn file_identity(path: &Path) -> Result<Vec<u8>, Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(path).map_err(|error| Error::Failed(error.to_string()))?;
        Ok([metadata.dev().to_le_bytes(), metadata.ino().to_le_bytes()].concat())
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let file = std::fs::File::open(path).map_err(|error| Error::Failed(error.to_string()))?;
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: the file handle is live and the output points to the API's
        // correctly sized writable structure. The handle is closed on return.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(Error::Failed(io::Error::last_os_error().to_string()));
        }
        Ok([
            info.dwVolumeSerialNumber.to_le_bytes(),
            info.nFileIndexHigh.to_le_bytes(),
            info.nFileIndexLow.to_le_bytes(),
        ]
        .concat())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Arc<Service>) {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("payload.bin"), b"partial").unwrap();
        std::fs::write(
            directory.path().join("source.torrent"),
            b"synthetic metadata",
        )
        .unwrap();
        let mut item = crate::client::tests::torrent();
        item.source = directory
            .path()
            .join("source.torrent")
            .to_string_lossy()
            .into_owned();
        item.overrides.download_directory = Some(directory.path().to_string_lossy().into_owned());
        item.files = FileSelectionModel::from_files([("payload.bin".into(), 100)]);
        let mut backend = crate::client::ClientBackend::with_state_store(
            crate::state::StateStore::in_memory().unwrap(),
        );
        backend
            .save_torrent(&crate::client::torrent_to_record(&item, None))
            .unwrap();
        let mut settings = ClientSettings::default();
        settings.download.download_directory = directory.path().to_string_lossy().into_owned();
        let service = Arc::new(Service {
            owner: Arc::new(Mutex::new(Controller::new(backend, vec![item]))),
            directory: directory.path().to_owned(),
            settings: Mutex::new(settings),
            saved_speed: Default::default(),
            history: Some(History::memory()),
            instance: "synthetic owner".into(),
            token_hash: "unused".into(),
            requested: Default::default(),
            request_wake: Default::default(),
            stop: Default::default(),
            wake: Default::default(),
            imports: tokio::sync::Semaphore::new(1),
            reviews: Default::default(),
            plans: Default::default(),
        });
        (directory, service)
    }
    fn prepare(service: &Service) -> TrashPlan {
        let revision = service
            .owner
            .lock()
            .unwrap()
            .command_revision("v1: abc")
            .unwrap();
        let Reply::TrashPlan(plan) = service
            .prepare_trash(
                "client".into(),
                "v1: abc".into(),
                revision,
                TrashScope::Everything,
            )
            .unwrap()
        else {
            panic!("missing plan")
        };
        plan
    }

    #[test]
    fn confirmation_is_bound_to_identity_but_allows_the_same_file_to_grow() {
        let (directory, service) = fixture();
        let plan = prepare(&service);
        assert_eq!(plan.count, 2);
        let prepared = service.plans.lock().unwrap();
        let context = prepared[&plan.plan_id].context.clone();
        drop(prepared);
        std::fs::write(
            directory.path().join("payload.bin"),
            b"more bytes of the same partial download",
        )
        .unwrap();
        assert_eq!(
            service
                .targets(&plan.id, plan.scope, &context)
                .unwrap()
                .digest,
            plan.digest
        );
        // Keeping the old file alive avoids relying on inode reuse behavior.
        std::fs::rename(
            directory.path().join("payload.bin"),
            directory.path().join("old-payload.bin"),
        )
        .unwrap();
        std::fs::write(
            directory.path().join("payload.bin"),
            b"replacement is a different file",
        )
        .unwrap();
        assert_ne!(
            service
                .targets(&plan.id, plan.scope, &context)
                .unwrap()
                .digest,
            plan.digest
        );
        let error = service
            .commit_trash("client".into(), plan.plan_id.clone(), plan.digest)
            .unwrap_err();
        assert!(error.to_string().contains("identities changed"));
        assert_eq!(
            std::fs::read(directory.path().join("payload.bin")).unwrap(),
            b"replacement is a different file"
        );
        assert!(!service.plans.lock().unwrap().contains_key(&plan.plan_id));
        assert!(service.owner.lock().unwrap().frontend_busy.is_empty());
    }

    #[test]
    fn plans_are_client_bound_single_use_expiring_and_storage_changes_fail_closed() {
        let (directory, service) = fixture();
        let plan = prepare(&service);
        assert!(matches!(
            service.commit_trash("another".into(), plan.plan_id.clone(), plan.digest.clone()),
            Err(Error::Stale)
        ));
        assert!(matches!(
            service.commit_trash("client".into(), plan.plan_id.clone(), "wrong digest".into()),
            Err(Error::Stale)
        ));
        assert!(service.plans.lock().unwrap().contains_key(&plan.plan_id));
        let elsewhere = directory.path().join("new location");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("payload.bin"), b"never confirmed").unwrap();
        service.owner.lock().unwrap().torrents[0]
            .overrides
            .download_directory = Some(elsewhere.to_string_lossy().into_owned());
        assert!(matches!(
            service.commit_trash("client".into(), plan.plan_id.clone(), plan.digest),
            Err(Error::Stale)
        ));
        assert_eq!(
            std::fs::read(elsewhere.join("payload.bin")).unwrap(),
            b"never confirmed"
        );
        assert!(directory.path().join("payload.bin").is_file());
        let expired = prepare(&service);
        service
            .plans
            .lock()
            .unwrap()
            .get_mut(&expired.plan_id)
            .unwrap()
            .until = Instant::now() - Duration::from_secs(1);
        assert!(matches!(
            service.commit_trash("client".into(), expired.plan_id, expired.digest),
            Err(Error::Stale)
        ));
    }

    #[test]
    fn plans_do_not_silently_skip_denied_or_escaping_payloads() {
        let (directory, service) = fixture();
        std::fs::remove_file(directory.path().join("payload.bin")).unwrap();
        std::fs::create_dir(directory.path().join("payload.bin")).unwrap();
        let revision = service
            .owner
            .lock()
            .unwrap()
            .command_revision("v1: abc")
            .unwrap();
        let error = service
            .prepare_trash(
                "client".into(),
                "v1: abc".into(),
                revision,
                TrashScope::DownloadedFiles,
            )
            .unwrap_err();
        assert!(error.to_string().contains("not a regular file"));
        assert!(service.plans.lock().unwrap().is_empty());
        assert_eq!(
            service
                .owner
                .lock()
                .unwrap()
                .backend
                .load_torrents()
                .unwrap()
                .len(),
            1
        );
    }
}
