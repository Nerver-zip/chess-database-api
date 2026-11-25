use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use chess_database_core::{Collection, ParsedGame};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::format::{GameBlock, StorageError};

pub const SHARD_MAGIC_HEADER: &[u8; 4] = b"CGCS"; // Game container shard header (named CODB in earlier releases)
pub const SHARD_MAGIC_FOOTER: &[u8; 4] = b"CGCE"; // Game container shard footer (named CODB in earlier releases)
pub const SHARD_FORMAT_VERSION: u32 = 1;
pub const SHARD_HEADER_SIZE: usize = 32;
pub const SHARD_FOOTER_SIZE: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BlockDescriptor {
    pub file_offset: u64,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
    pub game_count: u32,
    pub first_game_id: u64,
}

/// Writer for a single persistent CGB3 shard file.
pub struct ShardWriter {
    path: PathBuf,
    shard_id: u16,
    collection: Collection,
    file: BufWriter<File>,
    current_offset: u64,
    block_descriptors: Vec<BlockDescriptor>,
    total_games: u64,
}

impl ShardWriter {
    pub fn create(
        path: PathBuf,
        shard_id: u16,
        collection: Collection,
    ) -> Result<Self, StorageError> {
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

        // 32-byte header:
        // [0..4]: Magic b"CGCS"
        // [4..8]: Version u32 (1)
        // [8..10]: Shard ID u16
        // [10..11]: Collection u8 (1=MastersOtb, 2=EliteOnline)
        // [11..32]: Reserved zero padding (21 bytes)
        writer.write_all(SHARD_MAGIC_HEADER)?;
        writer.write_u32::<BigEndian>(SHARD_FORMAT_VERSION)?;
        writer.write_u16::<BigEndian>(shard_id)?;
        let col_byte = match collection {
            Collection::MastersOtb => 1u8,
            Collection::EliteOnline => 2u8,
        };
        writer.write_u8(col_byte)?;
        writer.write_all(&[0u8; 21])?;

        Ok(ShardWriter {
            path,
            shard_id,
            collection,
            file: writer,
            current_offset: SHARD_HEADER_SIZE as u64,
            block_descriptors: Vec::new(),
            total_games: 0,
        })
    }

    pub fn shard_id(&self) -> u16 {
        self.shard_id
    }

    pub fn collection(&self) -> Collection {
        self.collection
    }

    pub fn total_games(&self) -> u64 {
        self.total_games
    }

    pub fn block_count(&self) -> u32 {
        self.block_descriptors.len() as u32
    }

    pub fn current_file_size(&self) -> u64 {
        self.current_offset
    }

    /// Appends a serialized, compressed CGB3 block to this shard.
    pub fn append_block(
        &mut self,
        compressed_payload: &[u8],
        game_count: u32,
        first_game_id: u64,
        uncompressed_size: u32,
    ) -> Result<u32, StorageError> {
        let block_index = self.block_descriptors.len() as u32;
        let file_offset = self.current_offset;
        let compressed_size = compressed_payload.len() as u32;

        self.file.write_all(compressed_payload)?;
        self.current_offset += compressed_size as u64;
        self.total_games += game_count as u64;

        self.block_descriptors.push(BlockDescriptor {
            file_offset,
            compressed_size,
            uncompressed_size,
            game_count,
            first_game_id,
        });

        Ok(block_index)
    }

    /// Finalizes the shard by writing the block directory table and footer.
    pub fn finish(mut self) -> Result<(PathBuf, Vec<BlockDescriptor>, u64), StorageError> {
        let block_dir_offset = self.current_offset;
        let block_count = self.block_descriptors.len() as u32;

        // Write block directory entries (28 bytes per entry)
        for desc in &self.block_descriptors {
            self.file.write_u64::<BigEndian>(desc.file_offset)?;
            self.file.write_u32::<BigEndian>(desc.compressed_size)?;
            self.file.write_u32::<BigEndian>(desc.uncompressed_size)?;
            self.file.write_u32::<BigEndian>(desc.game_count)?;
            self.file.write_u64::<BigEndian>(desc.first_game_id)?;
        }

        // Write footer (20 bytes):
        // [0..8]: block_dir_offset u64
        // [8..12]: block_count u32
        // [12..16]: reserved u32 (0)
        // [16..20]: Magic b"CGCE"
        self.file.write_u64::<BigEndian>(block_dir_offset)?;
        self.file.write_u32::<BigEndian>(block_count)?;
        self.file.write_u32::<BigEndian>(0)?;
        self.file.write_all(SHARD_MAGIC_FOOTER)?;

        self.file.flush()?;
        let total_size = self
            .file
            .get_ref()
            .metadata()
            .map(|m| m.len())
            .unwrap_or(self.current_offset);

        Ok((self.path, self.block_descriptors, total_size))
    }
}

/// Reader for a persistent CGB3 shard file.
pub struct ShardReader {
    path: PathBuf,
    shard_id: u16,
    collection: Collection,
    file: File,
    block_descriptors: Vec<BlockDescriptor>,
    total_games: u64,
}

impl ShardReader {
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        let mut file = File::open(path)?;
        let file_len = file.metadata()?.len();

        if file_len < (SHARD_HEADER_SIZE + SHARD_FOOTER_SIZE) as u64 {
            return Err(StorageError::Decoding(format!(
                "Shard file '{}' is too small ({} bytes) to contain valid headers",
                path.display(),
                file_len
            )));
        }

        // Read and verify header
        let mut header = [0u8; SHARD_HEADER_SIZE];
        file.read_exact(&mut header)?;

        if &header[0..4] != SHARD_MAGIC_HEADER {
            return Err(StorageError::InvalidMagic);
        }

        let mut r = Cursor::new(&header[4..]);
        let version = r.read_u32::<BigEndian>()?;
        if version != SHARD_FORMAT_VERSION {
            return Err(StorageError::Decoding(format!(
                "Unsupported shard format version {} (expected {})",
                version, SHARD_FORMAT_VERSION
            )));
        }

        let shard_id = r.read_u16::<BigEndian>()?;
        let col_byte = r.read_u8()?;
        let collection = match col_byte {
            1 => Collection::MastersOtb,
            2 => Collection::EliteOnline,
            other => {
                return Err(StorageError::Decoding(format!(
                    "Invalid collection byte {} in shard header",
                    other
                )))
            }
        };

        // Seek to footer to read directory location
        file.seek(SeekFrom::End(-(SHARD_FOOTER_SIZE as i64)))?;
        let mut footer = [0u8; SHARD_FOOTER_SIZE];
        file.read_exact(&mut footer)?;

        if &footer[16..20] != SHARD_MAGIC_FOOTER {
            return Err(StorageError::Decoding(format!(
                "Corrupt shard file '{}': missing CGCE footer magic",
                path.display()
            )));
        }

        let mut rf = Cursor::new(&footer);
        let block_dir_offset = rf.read_u64::<BigEndian>()?;
        let block_count = rf.read_u32::<BigEndian>()? as usize;

        if block_dir_offset >= file_len || block_dir_offset < SHARD_HEADER_SIZE as u64 {
            return Err(StorageError::Decoding(format!(
                "Corrupt shard file '{}': invalid block directory offset {}",
                path.display(),
                block_dir_offset
            )));
        }

        // Read block descriptors
        file.seek(SeekFrom::Start(block_dir_offset))?;
        let mut block_descriptors = Vec::with_capacity(block_count);
        let mut total_games = 0u64;

        for _ in 0..block_count {
            let file_offset = file.read_u64::<BigEndian>()?;
            let compressed_size = file.read_u32::<BigEndian>()?;
            let uncompressed_size = file.read_u32::<BigEndian>()?;
            let game_count = file.read_u32::<BigEndian>()?;
            let first_game_id = file.read_u64::<BigEndian>()?;

            if file_offset + compressed_size as u64 > block_dir_offset {
                return Err(StorageError::Decoding(format!(
                    "Corrupt shard file '{}': block offset {} + size {} extends past directory",
                    path.display(),
                    file_offset,
                    compressed_size
                )));
            }

            total_games += game_count as u64;
            block_descriptors.push(BlockDescriptor {
                file_offset,
                compressed_size,
                uncompressed_size,
                game_count,
                first_game_id,
            });
        }

        Ok(ShardReader {
            path: path.to_path_buf(),
            shard_id,
            collection,
            file,
            block_descriptors,
            total_games,
        })
    }

    pub fn shard_id(&self) -> u16 {
        self.shard_id
    }

    pub fn collection(&self) -> Collection {
        self.collection
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn total_games(&self) -> u64 {
        self.total_games
    }

    pub fn block_count(&self) -> u32 {
        self.block_descriptors.len() as u32
    }

    pub fn block_descriptors(&self) -> &[BlockDescriptor] {
        &self.block_descriptors
    }

    /// Reads raw compressed bytes for a given block.
    pub fn read_block_bytes(&mut self, block_index: u32) -> Result<Vec<u8>, StorageError> {
        let desc = self
            .block_descriptors
            .get(block_index as usize)
            .ok_or_else(|| {
                StorageError::Decoding(format!(
                    "Block index {} out of bounds (shard has {} blocks)",
                    block_index,
                    self.block_descriptors.len()
                ))
            })?;

        self.file.seek(SeekFrom::Start(desc.file_offset))?;
        let mut buf = vec![0u8; desc.compressed_size as usize];
        self.file.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// Decompresses the specified block and returns all its games.
    pub fn decompress_block(&mut self, block_index: u32) -> Result<Vec<ParsedGame>, StorageError> {
        let compressed = self.read_block_bytes(block_index)?;
        GameBlock::decompress(&compressed, self.collection)
    }

    /// Retrieves an individual game by block index and game offset within the block.
    pub fn get_game(
        &mut self,
        block_index: u32,
        game_index_in_block: u16,
    ) -> Result<ParsedGame, StorageError> {
        let games = self.decompress_block(block_index)?;
        games
            .into_iter()
            .nth(game_index_in_block as usize)
            .ok_or_else(|| {
                StorageError::Decoding(format!(
                    "Game index {} out of bounds for block {} in shard {}",
                    game_index_in_block, block_index, self.shard_id
                ))
            })
    }
}
