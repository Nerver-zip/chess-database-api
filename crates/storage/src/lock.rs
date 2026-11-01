use crate::format::StorageError;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

/// Advisory dataset-scoped process lock to serialize mutations (publication, indexing, recovery)
/// while allowing concurrent snapshot readers.
#[derive(Debug)]
pub struct DatasetLock {
    #[allow(dead_code)]
    file: File,
    #[allow(dead_code)]
    path: PathBuf,
}

impl DatasetLock {
    /// Attempts to acquire an exclusive advisory lock on `<dataset_dir>/.dataset.lock`.
    ///
    /// Fails immediately with [`StorageError::Lock`] if another process holds the lock.
    pub fn try_acquire_exclusive(dataset_dir: &Path) -> Result<Self, StorageError> {
        let lock_path = dataset_dir.join(".dataset.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;

        #[cfg(unix)]
        {
            let res = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if res != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EWOULDBLOCK)
                    || err.raw_os_error() == Some(libc::EAGAIN)
                {
                    return Err(StorageError::Lock(format!(
                        "Dataset at '{}' is locked by another active process",
                        dataset_dir.display()
                    )));
                }
                return Err(StorageError::Io(err));
            }
        }

        Ok(Self {
            file,
            path: lock_path,
        })
    }

    /// Acquires an exclusive advisory lock on `<dataset_dir>/.dataset.lock`, blocking until acquired.
    pub fn acquire_exclusive(dataset_dir: &Path) -> Result<Self, StorageError> {
        let lock_path = dataset_dir.join(".dataset.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;

        #[cfg(unix)]
        {
            let res = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if res != 0 {
                return Err(StorageError::Io(std::io::Error::last_os_error()));
            }
        }

        Ok(Self {
            file,
            path: lock_path,
        })
    }
}

#[cfg(unix)]
impl Drop for DatasetLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Advisory per-staging-directory lock to prevent cleanup of active staging directories.
#[derive(Debug)]
pub struct StagingLock {
    #[allow(dead_code)]
    file: File,
    #[allow(dead_code)]
    path: PathBuf,
}

impl StagingLock {
    /// Creates and acquires an exclusive advisory lock on `<staging_dir>/.staging.lock`.
    pub fn acquire(staging_dir: &Path) -> Result<Self, StorageError> {
        fs::create_dir_all(staging_dir)?;
        let lock_path = staging_dir.join(".staging.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;

        #[cfg(unix)]
        {
            let res = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if res != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EWOULDBLOCK)
                    || err.raw_os_error() == Some(libc::EAGAIN)
                {
                    return Err(StorageError::Lock(format!(
                        "Staging directory at '{}' is locked by an active build process",
                        staging_dir.display()
                    )));
                }
                return Err(StorageError::Io(err));
            }
        }

        Ok(Self {
            file,
            path: lock_path,
        })
    }

    /// Checks whether an exclusive lock is currently held on `<staging_dir>/.staging.lock`.
    pub fn is_staging_locked(staging_dir: &Path) -> bool {
        let lock_path = staging_dir.join(".staging.lock");
        if !lock_path.exists() {
            return false;
        }

        #[cfg(unix)]
        {
            let file_res = OpenOptions::new().read(true).write(true).open(&lock_path);
            if let Ok(file) = file_res {
                let res = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
                if res != 0 {
                    let err = std::io::Error::last_os_error();
                    if err.raw_os_error() == Some(libc::EWOULDBLOCK)
                        || err.raw_os_error() == Some(libc::EAGAIN)
                    {
                        return true;
                    }
                } else {
                    let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
                }
            }
        }

        false
    }
}

#[cfg(unix)]
impl Drop for StagingLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Checks whether a process with the given PID is currently alive on the system.
pub fn is_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }

    #[cfg(unix)]
    {
        let res = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if res == 0 {
            true
        } else {
            let err = std::io::Error::last_os_error();
            err.raw_os_error() == Some(libc::EPERM)
        }
    }

    #[cfg(not(unix))]
    {
        false
    }
}

/// Attempts to parse the PID from a staging directory name like `.staging_index_<pid>_<timestamp>`
/// or `.staging_<pid>_<timestamp>`.
pub fn extract_pid_from_staging_name(name: &str) -> Option<u32> {
    let parts: Vec<&str> = name.split('_').collect();
    if parts.len() >= 3 && parts[0] == ".staging" && parts[1] == "index" {
        parts[2].parse::<u32>().ok()
    } else if parts.len() >= 2 && parts[0] == ".staging" {
        parts[1].parse::<u32>().ok()
    } else {
        None
    }
}
