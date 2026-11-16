use byteorder::{BigEndian, ByteOrder};
use memmap2::Mmap;
use opening_explorer_core::{Collection, GameLocation};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::format::StorageError;

pub const LOOKUP_MAGIC: &[u8; 4] = b"CGL1"; // Game lookup v1 (named CODB in earlier releases)
pub const LOOKUP_VERSION: u32 = 1;
pub const LOOKUP_HEADER_SIZE: usize = 32;
pub const LOOKUP_ENTRY_SIZE: usize = 16;
pub const LOOKUP_FOOTER_SIZE: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GameLookupEntry {
    pub game_id: u64,
    pub shard_id: u16,
    pub block_index: u32,
    pub game_index_in_block: u16,
}

impl GameLookupEntry {
    pub fn new(game_id: u64, shard_id: u16, block_index: u32, game_index_in_block: u16) -> Self {
        GameLookupEntry {
            game_id,
            shard_id,
            block_index,
            game_index_in_block,
        }
    }

    pub fn location(&self) -> GameLocation {
        GameLocation::new(self.shard_id, self.block_index, self.game_index_in_block)
    }
}

/// Writer for the persistent global game lookup index.
pub struct GameLookupWriter {
    path: PathBuf,
    collection: Collection,
    entries: Vec<GameLookupEntry>,
}

impl GameLookupWriter {
    pub fn new(path: PathBuf, collection: Collection) -> Self {
        GameLookupWriter {
            path,
            collection,
            entries: Vec::new(),
        }
    }

    pub fn with_capacity(path: PathBuf, collection: Collection, capacity: usize) -> Self {
        GameLookupWriter {
            path,
            collection,
            entries: Vec::with_capacity(capacity),
        }
    }

    pub fn add_entry(&mut self, game_id: u64, location: GameLocation) {
        self.entries.push(GameLookupEntry {
            game_id,
            shard_id: location.shard_id,
            block_index: location.block_index,
            game_index_in_block: location.game_index_in_block,
        });
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Sorts entries by game ID, writes header, entries, and SHA-256 verification hash.
    pub fn finish(mut self) -> Result<(PathBuf, u64, String), StorageError> {
        // Sort entries deterministically by (game_id, shard_id, block_index, game_index)
        self.entries.sort_unstable();
        let count = self.entries.len() as u64;
        Self::write_from_iter(
            self.path,
            self.collection,
            count,
            self.entries.into_iter().map(Ok),
        )
    }

    /// Streams sorted entries from an iterator directly into the persistent lookup file,
    /// writing the 32-byte header, 16-byte fixed entries, and 32-byte SHA-256 footer
    /// with strictly O(1) memory consumption.
    pub fn write_from_iter<I>(
        path: PathBuf,
        collection: Collection,
        count: u64,
        iter: I,
    ) -> Result<(PathBuf, u64, String), StorageError>
    where
        I: Iterator<Item = Result<GameLookupEntry, StorageError>>,
    {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)?;
        let mut writer = BufWriter::new(file);
        let mut hasher = Sha256::new();

        // 32-byte header:
        // [0..4]: Magic b"CGL1"
        // [4..8]: Version u32 (1)
        // [8..9]: Collection u8
        // [9..24]: Reserved zero padding (15 bytes)
        // [24..32]: Entry count u64
        let mut header = [0u8; LOOKUP_HEADER_SIZE];
        header[0..4].copy_from_slice(LOOKUP_MAGIC);
        BigEndian::write_u32(&mut header[4..8], LOOKUP_VERSION);
        header[8] = match collection {
            Collection::MastersOtb => 1,
            Collection::EliteOnline => 2,
        };
        BigEndian::write_u64(&mut header[24..32], count);

        writer.write_all(&header)?;
        hasher.update(header);

        // Entries: each 16 bytes:
        // [0..8]: game_id u64
        // [8..10]: shard_id u16
        // [10..14]: block_index u32
        // [14..16]: game_index_in_block u16
        let mut entry_buf = [0u8; LOOKUP_ENTRY_SIZE];
        let mut written_count = 0u64;
        let mut prev_entry: Option<GameLookupEntry> = None;

        for entry_res in iter {
            let entry = entry_res?;
            if let Some(prev) = prev_entry {
                if entry.game_id < prev.game_id {
                    return Err(StorageError::Integrity(format!(
                        "Lookup entries must be strictly sorted: game ID {} < previous {}",
                        entry.game_id, prev.game_id
                    )));
                }
                if entry == prev {
                    return Err(StorageError::Integrity(format!(
                        "Duplicate lookup entry encountered in lookup table: {:?}",
                        entry
                    )));
                }
            }
            prev_entry = Some(entry);

            BigEndian::write_u64(&mut entry_buf[0..8], entry.game_id);
            BigEndian::write_u16(&mut entry_buf[8..10], entry.shard_id);
            BigEndian::write_u32(&mut entry_buf[10..14], entry.block_index);
            BigEndian::write_u16(&mut entry_buf[14..16], entry.game_index_in_block);
            writer.write_all(&entry_buf)?;
            hasher.update(entry_buf);
            written_count += 1;
        }

        if written_count != count {
            return Err(StorageError::Integrity(format!(
                "Lookup entry count mismatch: expected {} entries, wrote {}",
                count, written_count
            )));
        }

        // 32-byte SHA-256 footer
        let checksum = hasher.finalize();
        writer.write_all(&checksum)?;
        writer.flush()?;

        let total_bytes = LOOKUP_HEADER_SIZE as u64
            + (count * LOOKUP_ENTRY_SIZE as u64)
            + LOOKUP_FOOTER_SIZE as u64;
        let sha256_hex = format!("{:064x}", checksum);

        Ok((path, total_bytes, sha256_hex))
    }
}

/// Memory-mapped reader for the persistent global game lookup index.
pub struct GameLookupReader {
    path: PathBuf,
    collection: Collection,
    entry_count: u64,
    mmap: Mmap,
}

impl GameLookupReader {
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        let file = File::open(path).map_err(|error| {
            StorageError::Io(std::io::Error::new(
                error.kind(),
                format!("{}: {}", path.display(), error),
            ))
        })?;
        let file_len = file.metadata()?.len();

        let min_len = (LOOKUP_HEADER_SIZE + LOOKUP_FOOTER_SIZE) as u64;
        if file_len < min_len {
            return Err(StorageError::Decoding(format!(
                "Lookup file '{}' too small ({} bytes)",
                path.display(),
                file_len
            )));
        }

        // Safety: Read-only memory mapping of an immutable finalized file.
        let mmap = unsafe { Mmap::map(&file)? };

        if &mmap[0..4] != LOOKUP_MAGIC {
            return Err(StorageError::InvalidMagic);
        }

        let version = BigEndian::read_u32(&mmap[4..8]);
        if version != LOOKUP_VERSION {
            return Err(StorageError::Decoding(format!(
                "Unsupported lookup version {} (expected {})",
                version, LOOKUP_VERSION
            )));
        }

        let col_byte = mmap[8];
        let collection = match col_byte {
            1 => Collection::MastersOtb,
            2 => Collection::EliteOnline,
            other => {
                return Err(StorageError::Decoding(format!(
                    "Invalid collection byte {} in lookup header",
                    other
                )))
            }
        };

        let entry_count = BigEndian::read_u64(&mmap[24..32]);
        let expected_size = LOOKUP_HEADER_SIZE as u64
            + (entry_count * LOOKUP_ENTRY_SIZE as u64)
            + LOOKUP_FOOTER_SIZE as u64;

        if file_len != expected_size {
            return Err(StorageError::Decoding(format!(
                "Lookup file size mismatch: actual {} bytes, expected {} for {} entries",
                file_len, expected_size, entry_count
            )));
        }

        Ok(GameLookupReader {
            path: path.to_path_buf(),
            collection,
            entry_count,
            mmap,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn collection(&self) -> Collection {
        self.collection
    }

    pub fn entry_count(&self) -> u64 {
        self.entry_count
    }

    #[inline]
    pub fn get_entry(&self, idx: usize) -> Option<GameLookupEntry> {
        if idx >= self.entry_count as usize {
            None
        } else {
            Some(self.get_entry_at(idx))
        }
    }

    #[inline]
    fn get_entry_at(&self, idx: usize) -> GameLookupEntry {
        let offset = LOOKUP_HEADER_SIZE + idx * LOOKUP_ENTRY_SIZE;
        let buf = &self.mmap[offset..offset + LOOKUP_ENTRY_SIZE];
        GameLookupEntry {
            game_id: BigEndian::read_u64(&buf[0..8]),
            shard_id: BigEndian::read_u16(&buf[8..10]),
            block_index: BigEndian::read_u32(&buf[10..14]),
            game_index_in_block: BigEndian::read_u16(&buf[14..16]),
        }
    }

    /// Finds all location entries for a given game ID via binary search.
    /// Handles ID collisions deterministically by returning all matching adjacent entries.
    pub fn find_locations(&self, target_game_id: u64) -> Vec<GameLookupEntry> {
        let count = self.entry_count as usize;
        if count == 0 {
            return Vec::new();
        }

        let mut low = 0usize;
        let mut high = count;

        while low < high {
            let mid = low + (high - low) / 2;
            let entry = self.get_entry_at(mid);
            if entry.game_id < target_game_id {
                low = mid + 1;
            } else {
                high = mid;
            }
        }

        let mut matches = Vec::new();
        let mut idx = low;
        while idx < count {
            let entry = self.get_entry_at(idx);
            if entry.game_id == target_game_id {
                matches.push(entry);
                idx += 1;
            } else {
                break;
            }
        }

        matches
    }

    /// Verifies the SHA-256 integrity checksum stored in the file footer.
    pub fn verify_checksum(&self) -> Result<bool, StorageError> {
        let data_len = self.mmap.len() - LOOKUP_FOOTER_SIZE;
        let mut hasher = Sha256::new();
        hasher.update(&self.mmap[..data_len]);
        let calculated = hasher.finalize();
        let stored = &self.mmap[data_len..];
        Ok(calculated.as_slice() == stored)
    }
}
