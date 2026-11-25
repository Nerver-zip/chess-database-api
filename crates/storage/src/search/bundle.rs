//! Local companion bundles for moving search indexes between identical snapshots.

use super::{
    canonical_search_fingerprint, publish_index_metadata, SearchIndexMetadata, CATALOG_FILENAME,
    POSTINGS_FILENAME, SEARCH_DIRECTORY, SEARCH_FORMAT_VERSION,
};
use crate::{
    compute_file_sha256, resolve_dataset_snapshot, sync_dir_contents, sync_dir_itself,
    verify_dataset, DatasetLock, DatasetReader, StagingLock, StorageError,
};
use std::collections::BTreeSet;
use std::ffi::{CString, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

const METADATA_FILENAME: &str = "search-index.json";
const MAX_METADATA_BYTES: u64 = 1024 * 1024;

/// Copy the current snapshot's verified search index into a new standalone directory.
///
/// The output directory must not exist. Files are copied so a bundle owner cannot mutate the
/// immutable snapshot through a hard link. The completed directory is installed with one rename.
pub fn pack_search_index(dataset: &Path, output: &Path) -> Result<(), StorageError> {
    let dataset = fs::canonicalize(dataset)?;
    let source = pin_current_snapshot(&dataset)?;
    verify_dataset(&source, true)?;
    let reader = DatasetReader::open(&source)?;
    let metadata =
        reader.manifest().search_index.as_ref().ok_or_else(|| {
            StorageError::Integrity("Dataset has no search index to bundle".into())
        })?;
    validate_metadata(metadata, reader.manifest())?;

    let (candidate_parent, output_name) = resolve_new_output_parent(output)?;
    let versions = fs::canonicalize(dataset.join("versions"))?;
    if candidate_parent.starts_with(&source) || candidate_parent.starts_with(&versions) {
        return Err(StorageError::Encoding(
            "Search bundle output must be outside immutable dataset snapshots".into(),
        ));
    }
    fs::create_dir_all(&candidate_parent)?;
    let output_parent = fs::canonicalize(&candidate_parent)?;
    if output_parent.starts_with(&source) || output_parent.starts_with(&versions) {
        return Err(StorageError::Encoding(
            "Search bundle output must be outside immutable dataset snapshots".into(),
        ));
    }
    let output_path = output_parent.join(output_name);
    match fs::symlink_metadata(&output_path) {
        Ok(_) => {
            return Err(StorageError::Integrity(format!(
                "Search bundle output '{}' already exists",
                output_path.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let stage = tempfile::Builder::new()
        .prefix(".staging_search_bundle_")
        .tempdir_in(&output_parent)?;
    let source_search = source.join(SEARCH_DIRECTORY);
    let target_search = stage.path().join(SEARCH_DIRECTORY);
    fs::create_dir(&target_search)?;
    for (name, artifact) in &metadata.artifacts {
        let source_path = source_search.join(name);
        ensure_regular_file(&source_path, "source search artifact")?;
        let target_path = target_search.join(name);
        fs::copy(&source_path, &target_path)?;
        verify_file(&target_path, artifact, name)?;
    }
    let metadata_bytes = serde_json::to_vec_pretty(metadata)
        .map_err(|error| StorageError::Encoding(error.to_string()))?;
    fs::write(stage.path().join(METADATA_FILENAME), metadata_bytes)?;
    sync_dir_contents(stage.path())?;

    rename_directory_noclobber(stage.path(), &output_path)?;
    // TempDir still tracks the old staging path; after a successful rename that path is absent.
    sync_dir_itself(&output_parent)
}

/// Install a local search bundle onto the current dataset when canonical game identity matches.
///
/// Installation takes the dataset lock, pins the current immutable version, copies only the
/// declared search artifacts into a fresh staging version, and delegates manifest creation, deep
/// verification, and atomic publication to `publish_index_metadata`.
pub fn install_search_index(dataset: &Path, bundle: &Path) -> Result<String, StorageError> {
    let dataset = fs::canonicalize(dataset)?;
    let _dataset_lock = DatasetLock::try_acquire_exclusive(&dataset)?;
    let source = pin_current_snapshot(&dataset)?;
    verify_dataset(&source, false)?;
    let reader = DatasetReader::open(&source)?;

    let (metadata, bundle_search) = read_bundle(bundle, reader.manifest())?;
    let versions = dataset.join("versions");
    fs::create_dir_all(&versions)?;
    let stage = tempfile::Builder::new()
        .prefix(".staging_search_")
        .tempdir_in(&versions)?;
    let _staging_lock = StagingLock::acquire(stage.path())?;
    let target_search = stage.path().join(SEARCH_DIRECTORY);
    fs::create_dir(&target_search)?;

    for (name, artifact) in &metadata.artifacts {
        let input = openat_regular(&bundle_search, name)?;
        let target = target_search.join(name);
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        let read_limit = artifact.file_size_bytes.checked_add(1).ok_or_else(|| {
            StorageError::Integrity(format!("Search artifact '{name}' size overflows"))
        })?;
        let copied = std::io::copy(&mut input.take(read_limit), &mut output)?;
        if copied != artifact.file_size_bytes {
            return Err(StorageError::Integrity(format!(
                "Search artifact '{name}' size changed while installing"
            )));
        }
        output.flush()?;
        output.sync_all()?;
        verify_file(&target, artifact, name)?;
    }

    let (build_id, _verification_seconds) =
        publish_index_metadata(&dataset, &reader, stage.path(), metadata)?;
    Ok(build_id)
}

fn pin_current_snapshot(dataset: &Path) -> Result<PathBuf, StorageError> {
    let current = dataset.join("current");
    let current_type = fs::symlink_metadata(&current)?;
    if !current_type.file_type().is_symlink() {
        return Err(StorageError::Integrity(
            "Search bundles require a versioned dataset with a current symlink".into(),
        ));
    }
    ensure_real_directory(&dataset.join("versions"), "Dataset versions directory")?;
    let versions = fs::canonicalize(dataset.join("versions"))?;
    let snapshot = resolve_dataset_snapshot(dataset)?;
    if !snapshot.starts_with(&versions) {
        return Err(StorageError::Integrity(
            "Dataset current symlink does not point inside versions/".into(),
        ));
    }
    Ok(snapshot)
}

fn resolve_new_output_parent(output: &Path) -> Result<(PathBuf, OsString), StorageError> {
    let name = output.file_name().ok_or_else(|| {
        StorageError::Encoding("Search bundle output must name a new directory".into())
    })?;
    if name.is_empty() || name == "." || name == ".." {
        return Err(StorageError::Encoding(
            "Search bundle output must name a new directory".into(),
        ));
    }
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = if parent.is_absolute() {
        parent.to_path_buf()
    } else {
        std::env::current_dir()?.join(parent)
    };
    if parent
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Ok((fs::canonicalize(parent)?, name.to_os_string()));
    }
    let mut normalized = PathBuf::new();
    for component in parent.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(Path::new("/")),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::Normal(part) => normalized.push(part),
        }
    }

    let mut unresolved = normalized;
    let mut suffix = Vec::new();
    loop {
        match fs::symlink_metadata(&unresolved) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let part = unresolved.file_name().ok_or_else(|| {
                    StorageError::Encoding("Search bundle output parent is invalid".into())
                })?;
                suffix.push(part.to_os_string());
                if !unresolved.pop() {
                    return Err(StorageError::Encoding(
                        "Search bundle output parent is invalid".into(),
                    ));
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    let mut resolved = fs::canonicalize(&unresolved)?;
    for part in suffix.into_iter().rev() {
        resolved.push(part);
    }
    Ok((resolved, name.to_os_string()))
}

fn validate_metadata(
    metadata: &SearchIndexMetadata,
    manifest: &crate::DatasetManifest,
) -> Result<(), StorageError> {
    if metadata.format_version != SEARCH_FORMAT_VERSION
        || u64::from(metadata.game_count) != manifest.total_games
        || metadata.canonical_sha256 != canonical_search_fingerprint(manifest)
        || metadata.source_snapshot.is_empty()
        || metadata.sort_buffer_bytes < 24
        || metadata.coverage.max_ply() == Some(0)
        || metadata.position_count > metadata.posting_count
        || metadata.posting_count < u64::from(metadata.game_count)
    {
        return Err(StorageError::Integrity(
            "Search bundle version, coverage, counts or canonical identity do not match the dataset".into(),
        ));
    }
    let expected = BTreeSet::from([CATALOG_FILENAME, POSTINGS_FILENAME]);
    let actual: BTreeSet<_> = metadata.artifacts.keys().map(String::as_str).collect();
    if actual != expected {
        return Err(StorageError::Integrity(
            "Search bundle contains an unexpected artifact set".into(),
        ));
    }
    for (name, artifact) in &metadata.artifacts {
        if name.is_empty()
            || name.starts_with('.')
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
            || artifact.file_size_bytes == 0
            || artifact.sha256.len() != 64
            || !artifact
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(StorageError::Integrity(
                "Search bundle has invalid artifact metadata".into(),
            ));
        }
    }
    metadata.file_size_bytes()?;
    Ok(())
}

fn read_bundle(
    bundle: &Path,
    manifest: &crate::DatasetManifest,
) -> Result<(SearchIndexMetadata, File), StorageError> {
    let name = bundle
        .file_name()
        .ok_or_else(|| StorageError::Encoding("Search bundle path must name a directory".into()))?;
    let parent = bundle
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let bundle = fs::canonicalize(parent)?.join(name);
    ensure_real_directory(&bundle, "Search bundle")?;
    let bundle_fd = open_directory_nofollow(&bundle)?;
    require_exact_entries(&bundle, &[METADATA_FILENAME, SEARCH_DIRECTORY])?;

    let metadata_file = openat_regular(&bundle_fd, METADATA_FILENAME)?;
    if metadata_file.metadata()?.len() > MAX_METADATA_BYTES {
        return Err(StorageError::Integrity(
            "Search bundle metadata file is too large".into(),
        ));
    }
    let mut bytes = Vec::new();
    metadata_file
        .take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(StorageError::Integrity(
            "Search bundle metadata file is too large".into(),
        ));
    }
    let metadata: SearchIndexMetadata = serde_json::from_slice(&bytes)
        .map_err(|error| StorageError::Decoding(format!("Invalid search-index.json: {error}")))?;
    validate_metadata(&metadata, manifest)?;

    let search_path = bundle.join(SEARCH_DIRECTORY);
    ensure_real_directory(&search_path, "Search artifact directory")?;
    let search_fd = openat_directory(&bundle_fd, SEARCH_DIRECTORY)?;
    let expected: Vec<_> = metadata.artifacts.keys().map(String::as_str).collect();
    require_exact_entries(&search_path, &expected)?;
    for (name, artifact) in &metadata.artifacts {
        let mut file = openat_regular(&search_fd, name)?;
        if file.metadata()?.len() != artifact.file_size_bytes {
            return Err(StorageError::Integrity(format!(
                "Search artifact '{name}' size mismatch"
            )));
        }
        if sha256_file(&mut file)? != artifact.sha256 {
            return Err(StorageError::Integrity(format!(
                "Search artifact '{name}' checksum mismatch"
            )));
        }
    }
    Ok((metadata, search_fd))
}

fn ensure_real_directory(path: &Path, label: &str) -> Result<(), StorageError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() {
        return Err(StorageError::Integrity(format!(
            "{label} must be a real directory, not a symlink"
        )));
    }
    Ok(())
}

fn ensure_regular_file(path: &Path, label: &str) -> Result<(), StorageError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(StorageError::Integrity(format!(
            "{label} must be a regular file, not a symlink or special file"
        )));
    }
    Ok(())
}

fn require_exact_entries(directory: &Path, expected: &[&str]) -> Result<(), StorageError> {
    let mut actual = BTreeSet::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|_| {
            StorageError::Integrity("Search bundle contains a non-UTF-8 filename".into())
        })?;
        if entry.file_type()?.is_symlink() {
            return Err(StorageError::Integrity(
                "Search bundle contains a symbolic link".into(),
            ));
        }
        actual.insert(name);
    }
    let expected: BTreeSet<_> = expected.iter().map(|name| (*name).to_owned()).collect();
    if actual != expected {
        return Err(StorageError::Integrity(
            "Search bundle contains missing or undeclared files".into(),
        ));
    }
    Ok(())
}

fn verify_file(
    path: &Path,
    artifact: &super::SearchArtifact,
    name: &str,
) -> Result<(), StorageError> {
    ensure_regular_file(path, "Search artifact")?;
    let actual_size = fs::metadata(path)?.len();
    if actual_size != artifact.file_size_bytes || compute_file_sha256(path)? != artifact.sha256 {
        return Err(StorageError::Integrity(format!(
            "Search artifact '{name}' does not match its declared size and checksum"
        )));
    }
    Ok(())
}

fn sha256_file(file: &mut File) -> Result<String, StorageError> {
    use sha2::{Digest, Sha256};

    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn open_directory_nofollow(path: &Path) -> Result<File, StorageError> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| StorageError::Encoding("Search bundle path contains NUL".into()))?;
    // SAFETY: `path` is a valid NUL-terminated path, and the returned descriptor is owned below.
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: `fd` is newly returned by open and has not been wrapped elsewhere.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn openat_directory(parent: &File, name: &str) -> Result<File, StorageError> {
    openat(
        parent,
        name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
    )
}

fn openat_regular(parent: &File, name: &str) -> Result<File, StorageError> {
    openat(
        parent,
        name,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
    )
}

fn openat(parent: &File, name: &str, flags: i32) -> Result<File, StorageError> {
    let name = CString::new(name)
        .map_err(|_| StorageError::Encoding("Search bundle filename contains NUL".into()))?;
    // SAFETY: `parent` is an open directory, `name` is a single NUL-terminated filename, and the
    // returned descriptor is converted to an owning `File` exactly once.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: `fd` is newly returned by openat and has not been wrapped elsewhere.
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if flags & libc::O_DIRECTORY != 0 {
        if !metadata.is_dir() {
            return Err(StorageError::Integrity(
                "Search bundle entry is not a directory".into(),
            ));
        }
    } else if !metadata.is_file() {
        return Err(StorageError::Integrity(
            "Search bundle artifact is not a regular file".into(),
        ));
    }
    Ok(file)
}

fn rename_directory_noclobber(source: &Path, destination: &Path) -> Result<(), StorageError> {
    if fs::symlink_metadata(destination).is_ok() {
        return Err(StorageError::Integrity(format!(
            "Search bundle output '{}' already exists",
            destination.display()
        )));
    }

    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let source = CString::new(source.as_os_str().as_bytes())
            .map_err(|_| StorageError::Encoding("Search bundle path contains NUL".into()))?;
        let destination = CString::new(destination.as_os_str().as_bytes())
            .map_err(|_| StorageError::Encoding("Search bundle path contains NUL".into()))?;
        // SAFETY: both strings are valid NUL-terminated paths on the same filesystem.
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    {
        fs::rename(source, destination)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::build_search_index;
    use crate::{verify_dataset, DatasetWriter, DatasetWriterConfig, SourceArchiveMetadata};
    use opening_explorer_core::{
        Collection, CompactMove, GameMetadata, GameResult, IngestionStats, ParsedGame,
    };
    use std::os::unix::fs::{symlink, MetadataExt};

    fn build_search_dataset(dataset: &Path) {
        let mut writer = DatasetWriter::new(
            dataset.to_path_buf(),
            DatasetWriterConfig {
                collection: Collection::MastersOtb,
                block_capacity: 4,
                max_games_per_shard: 10,
                zstd_level: 1,
                ..Default::default()
            },
        )
        .unwrap();
        writer
            .push_game(ParsedGame {
                metadata: GameMetadata {
                    id: 1,
                    collection: Collection::MastersOtb,
                    white: "Alice Example".into(),
                    black: "Bob Example".into(),
                    white_elo: Some(2400),
                    black_elo: Some(2300),
                    date: Some("2024.02.29".into()),
                    event: Some("Spring Open".into()),
                    site: Some("Oslo".into()),
                    round: Some("1".into()),
                    result: GameResult::WhiteWins,
                    eco: Some("C20".into()),
                    time_control: Some("600+0".into()),
                    source_id: None,
                    broadcast_url: None,
                    source_attribution: None,
                },
                moves: vec![CompactMove::new(12, 28, None)],
                fen_start: None,
            })
            .unwrap();
        writer
            .finalize(
                IngestionStats {
                    total_encountered: 1,
                    accepted_unique: 1,
                    ..Default::default()
                },
                vec![SourceArchiveMetadata {
                    filename: "fixture.pgn".into(),
                    url: None,
                    sha256: Some("00".repeat(32)),
                    size_bytes: 1,
                    license: "CC0-1.0".into(),
                    checksum_verified: Some(true),
                }],
            )
            .unwrap();
        build_search_index(dataset, Default::default()).unwrap();
    }

    #[test]
    fn pack_and_install_search_bundle_copies_artifacts_and_publishes() {
        let temporary = tempfile::tempdir().unwrap();
        let dataset = temporary.path().join("dataset");
        build_search_dataset(&dataset);
        let source = resolve_dataset_snapshot(&dataset).unwrap();
        let forbidden_output = source.join("not-created").join("bundle");
        assert!(pack_search_index(&dataset, &forbidden_output).is_err());
        assert!(!source.join("not-created").exists());
        let bundle = temporary.path().join("bundle");
        pack_search_index(&dataset, &bundle).unwrap();

        let before = resolve_dataset_snapshot(&dataset).unwrap();
        let metadata: SearchIndexMetadata =
            serde_json::from_slice(&fs::read(bundle.join(METADATA_FILENAME)).unwrap()).unwrap();
        for name in metadata.artifacts.keys() {
            let source = before.join(SEARCH_DIRECTORY).join(name);
            let packaged = bundle.join(SEARCH_DIRECTORY).join(name);
            assert_ne!(
                fs::metadata(source).unwrap().ino(),
                fs::metadata(packaged).unwrap().ino(),
                "bundle artifacts must be copies, not hard links"
            );
        }

        let build_id = install_search_index(&dataset, &bundle).unwrap();
        assert!(build_id.ends_with("-search"));
        verify_dataset(&dataset, true).unwrap();
    }

    #[test]
    fn install_rejects_artifact_symlinks_without_publishing() {
        let temporary = tempfile::tempdir().unwrap();
        let dataset = temporary.path().join("dataset");
        build_search_dataset(&dataset);
        let valid_bundle = temporary.path().join("valid-bundle");
        pack_search_index(&dataset, &valid_bundle).unwrap();
        let invalid_bundle = temporary.path().join("invalid-bundle");
        fs::create_dir(&invalid_bundle).unwrap();
        fs::copy(
            valid_bundle.join(METADATA_FILENAME),
            invalid_bundle.join(METADATA_FILENAME),
        )
        .unwrap();
        fs::create_dir(invalid_bundle.join(SEARCH_DIRECTORY)).unwrap();
        let metadata: SearchIndexMetadata =
            serde_json::from_slice(&fs::read(valid_bundle.join(METADATA_FILENAME)).unwrap())
                .unwrap();
        for name in metadata.artifacts.keys() {
            symlink(
                valid_bundle.join(SEARCH_DIRECTORY).join(name),
                invalid_bundle.join(SEARCH_DIRECTORY).join(name),
            )
            .unwrap();
        }

        let before = resolve_dataset_snapshot(&dataset).unwrap();
        assert!(install_search_index(&dataset, &invalid_bundle).is_err());
        let after = resolve_dataset_snapshot(&dataset).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn recovery_removes_abandoned_search_stage_and_preserves_current() {
        let temporary = tempfile::tempdir().unwrap();
        let dataset = temporary.path().join("dataset");
        build_search_dataset(&dataset);
        let before = resolve_dataset_snapshot(&dataset).unwrap();
        let stage = dataset.join("versions/.staging_search_interrupted");
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("partial.postings"), b"incomplete").unwrap();
        let lock = StagingLock::acquire(&stage).unwrap();

        crate::recover_dataset(&dataset).unwrap();
        assert!(stage.exists());
        drop(lock);

        crate::recover_dataset(&dataset).unwrap();
        assert!(!stage.exists());
        assert_eq!(resolve_dataset_snapshot(&dataset).unwrap(), before);
        verify_dataset(&dataset, true).unwrap();
    }
}
