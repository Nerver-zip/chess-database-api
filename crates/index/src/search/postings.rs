//! Sorted, grouped position-to-game occurrence postings.
//!
//! The artifact keeps a fixed-width sparse directory followed by compact posting
//! streams. Each directory entry names one canonical position key and the byte
//! offset of its stream. Within a stream, game ordinals are gap-coded and plies
//! are encoded as unsigned variable integers. Played moves use a big-endian
//! `u16`, with `0xffff` reserved for a terminal occurrence.

use crate::position::IndexError;
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use memmap2::Mmap;
use opening_explorer_core::{CompactMove, PositionKey};
use std::fs::{self, File};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"CHSPST01";
const VERSION: u32 = 1;
const HEADER_SIZE: u64 = 64;
const DIRECTORY_ENTRY_SIZE: u64 = 24;
const HEADER_LEN: usize = HEADER_SIZE as usize;
const DIRECTORY_LEN: usize = DIRECTORY_ENTRY_SIZE as usize;
const TERMINAL_MOVE: u16 = u16::MAX;
const MAX_VARINT_BYTES: usize = 5;
const CHECKPOINT_STRIDE: u32 = 256;
const CHECKPOINT_ENTRY_SIZE: u64 = 24;

/// One legal occurrence of a canonical position in a game.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchPosting {
    pub key: PositionKey,
    pub ordinal: u32,
    pub ply: u16,
    pub played: Option<CompactMove>,
}

/// Measurements collected while writing a sorted postings artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchPostingsBuildStats {
    pub position_count: u64,
    pub posting_count: u64,
    pub game_count: u64,
    pub file_size_bytes: u64,
    pub temporary_spool_bytes: u64,
}

/// Streaming writer for a sorted position-postings artifact.
pub struct SearchPostingsWriter {
    path: PathBuf,
    parent: PathBuf,
    game_count: u64,
    directory_spool: BufWriter<File>,
    posting_spool: BufWriter<File>,
    checkpoint_spool: BufWriter<File>,
    position_count: u64,
    posting_count: u64,
    current_key: Option<u128>,
    previous_record: Option<(u128, u32, u16)>,
    previous_ordinal: u32,
    previous_ply: u16,
    current_position_index: u64,
    current_position_postings: u32,
    payload_len: u64,
}

#[derive(Clone, Copy, Debug)]
struct DirectoryEntry {
    key: u128,
    offset: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CheckpointEntry {
    position_index: u64,
    posting_index: u32,
    ordinal: u32,
    offset: u64,
}

/// Writes sorted postings to a temporary payload, then assembles and atomically
/// renames the versioned mmap-friendly artifact at `path`.
///
/// Input must be strictly ordered by `(key, ordinal, ply)`. Each ordinal must be
/// less than `game_count`. The writer retains only one directory entry in its
/// directory spool and one posting in memory at a time.
pub fn write_search_postings<I>(
    path: impl AsRef<Path>,
    game_count: u64,
    records: I,
) -> Result<SearchPostingsBuildStats, IndexError>
where
    I: IntoIterator<Item = Result<SearchPosting, IndexError>>,
{
    let path = path.as_ref().to_path_buf();
    let spool_dir = path.parent().unwrap_or(Path::new("."));
    let mut writer = SearchPostingsWriter::create(&path, game_count, spool_dir)?;
    for record in records {
        writer.push(record?)?;
    }
    writer.finish()
}

impl SearchPostingsWriter {
    pub fn create(
        path: impl AsRef<Path>,
        game_count: u64,
        temporary_directory: impl AsRef<Path>,
    ) -> Result<Self, IndexError> {
        if game_count > u64::from(u32::MAX) + 1 {
            return Err(invalid(
                "game_count exceeds the 32-bit snapshot ordinal capacity",
            ));
        }
        let path = path.as_ref().to_path_buf();
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf();
        fs::create_dir_all(&parent)?;
        fs::create_dir_all(temporary_directory.as_ref())?;
        let temporary_directory = temporary_directory.as_ref();
        Ok(Self {
            path,
            directory_spool: BufWriter::new(tempfile::tempfile_in(temporary_directory)?),
            posting_spool: BufWriter::new(tempfile::tempfile_in(temporary_directory)?),
            checkpoint_spool: BufWriter::new(tempfile::tempfile_in(temporary_directory)?),
            parent,
            game_count,
            position_count: 0,
            posting_count: 0,
            current_key: None,
            previous_record: None,
            previous_ordinal: 0,
            previous_ply: 0,
            current_position_index: 0,
            current_position_postings: 0,
            payload_len: 0,
        })
    }

    pub fn push(&mut self, record: SearchPosting) -> Result<(), IndexError> {
        let key = record.key.0;
        if u64::from(record.ordinal) >= self.game_count {
            return Err(invalid(format!(
                "posting ordinal {} is outside game_count {}",
                record.ordinal, self.game_count
            )));
        }
        if record.played.is_some_and(|mv| mv.0 == TERMINAL_MOVE) {
            return Err(invalid("0xffff is reserved for terminal occurrences"));
        }

        if let Some((previous_key, previous_game, previous_record_ply)) = self.previous_record {
            if (key, record.ordinal, record.ply)
                <= (previous_key, previous_game, previous_record_ply)
            {
                return Err(invalid(format!(
                    "postings are not strictly ordered at ({key:032x}, {}, {})",
                    record.ordinal, record.ply
                )));
            }
        }

        if self.current_key != Some(key) {
            if let Some((previous_key, _, _)) = self.previous_record {
                if key <= previous_key {
                    return Err(invalid("position keys must be strictly increasing"));
                }
            }
            self.directory_spool.write_u128::<BigEndian>(key)?;
            self.directory_spool
                .write_u64::<BigEndian>(self.payload_len)?;
            self.position_count = self
                .position_count
                .checked_add(1)
                .ok_or_else(|| IndexError::Overflow("search position count overflow".into()))?;
            self.current_key = Some(key);
            self.current_position_index = self.position_count - 1;
            self.current_position_postings = 0;
            self.previous_ordinal = 0;
            self.previous_ply = 0;
        }

        let record_offset = self.payload_len;
        let at_checkpoint = self
            .current_position_postings
            .checked_rem(CHECKPOINT_STRIDE)
            == Some(0);
        let same_ordinal = self
            .previous_record
            .is_some_and(|(prev_key, prev_ordinal, _)| {
                prev_key == key && prev_ordinal == record.ordinal
            });
        let ordinal_value = if at_checkpoint {
            write_varint(&mut self.posting_spool, record.ordinal)?;
            record.ordinal
        } else if self
            .previous_record
            .is_some_and(|(prev_key, _, _)| prev_key == key)
        {
            if record.ordinal < self.previous_ordinal {
                return Err(invalid("game ordinals must increase within each position"));
            }
            if record.ordinal == self.previous_ordinal {
                if record.ply <= self.previous_ply {
                    return Err(invalid(
                        "plies must strictly increase within each game and position",
                    ));
                }
                write_varint(&mut self.posting_spool, 0)?;
            } else {
                write_varint(
                    &mut self.posting_spool,
                    record.ordinal - self.previous_ordinal,
                )?;
            }
            record.ordinal - self.previous_ordinal
        } else {
            write_varint(&mut self.posting_spool, record.ordinal)?;
            record.ordinal
        };
        if at_checkpoint {
            write_varint(&mut self.posting_spool, u32::from(record.ply))?;
        } else if same_ordinal {
            write_varint(
                &mut self.posting_spool,
                u32::from(record.ply - self.previous_ply),
            )?;
        } else {
            write_varint(&mut self.posting_spool, u32::from(record.ply))?;
        }
        self.posting_spool
            .write_u16::<BigEndian>(record.played.map_or(TERMINAL_MOVE, |mv| mv.0))?;

        // The directory already points at the first occurrence in each group.
        if at_checkpoint && self.current_position_postings > 0 {
            self.checkpoint_spool
                .write_u64::<BigEndian>(self.current_position_index)?;
            self.checkpoint_spool
                .write_u32::<BigEndian>(self.current_position_postings)?;
            self.checkpoint_spool
                .write_u32::<BigEndian>(ordinal_value)?;
            self.checkpoint_spool
                .write_u64::<BigEndian>(record_offset)?;
        }
        self.payload_len = self
            .payload_len
            .checked_add(varint_len(ordinal_value) as u64)
            .and_then(|len| {
                len.checked_add(varint_len(if at_checkpoint || !same_ordinal {
                    u32::from(record.ply)
                } else {
                    u32::from(record.ply - self.previous_ply)
                }) as u64)
            })
            .and_then(|len| len.checked_add(2))
            .ok_or_else(|| IndexError::Overflow("postings payload size overflow".into()))?;
        self.current_position_postings = self
            .current_position_postings
            .checked_add(1)
            .ok_or_else(|| IndexError::Overflow("per-position posting count overflow".into()))?;

        self.previous_ordinal = record.ordinal;
        self.previous_ply = record.ply;
        self.previous_record = Some((key, record.ordinal, record.ply));
        self.posting_count = self
            .posting_count
            .checked_add(1)
            .ok_or_else(|| IndexError::Overflow("search posting count overflow".into()))?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<SearchPostingsBuildStats, IndexError> {
        self.directory_spool.flush()?;
        self.posting_spool.flush()?;
        self.checkpoint_spool.flush()?;

        let directory_bytes = self
            .position_count
            .checked_mul(DIRECTORY_ENTRY_SIZE)
            .ok_or_else(|| {
                IndexError::Overflow("search postings directory size overflow".into())
            })?;
        let postings_offset = HEADER_SIZE
            .checked_add(directory_bytes)
            .ok_or_else(|| IndexError::Overflow("search postings data offset overflow".into()))?;
        let payload_bytes = self.posting_spool.get_ref().metadata()?.len();
        let checkpoint_offset = postings_offset
            .checked_add(payload_bytes)
            .ok_or_else(|| IndexError::Overflow("checkpoint offset overflow".into()))?;
        let checkpoint_bytes = self.checkpoint_spool.get_ref().metadata()?.len();
        let temporary_spool_bytes = self
            .directory_spool
            .get_ref()
            .metadata()?
            .len()
            .checked_add(self.posting_spool.get_ref().metadata()?.len())
            .and_then(|bytes| {
                bytes.checked_add(self.checkpoint_spool.get_ref().metadata().ok()?.len())
            })
            .ok_or_else(|| IndexError::Overflow("temporary spool size overflow".into()))?;
        let file_size_bytes = checkpoint_offset
            .checked_add(checkpoint_bytes)
            .ok_or_else(|| IndexError::Overflow("search postings file size overflow".into()))?;

        let assembled = tempfile::NamedTempFile::new_in(&self.parent)?;
        let (mut output, temp_path) = assembled.into_parts();
        write_header(
            &mut output,
            self.position_count,
            self.posting_count,
            self.game_count,
            checkpoint_offset,
            file_size_bytes,
        )?;
        self.directory_spool.get_mut().seek(SeekFrom::Start(0))?;
        self.posting_spool.get_mut().seek(SeekFrom::Start(0))?;
        self.checkpoint_spool.get_mut().seek(SeekFrom::Start(0))?;
        {
            let directory_reader = self.directory_spool.get_mut();
            for _ in 0..self.position_count {
                let key = directory_reader.read_u128::<BigEndian>()?;
                let relative_offset = directory_reader.read_u64::<BigEndian>()?;
                output.write_u128::<BigEndian>(key)?;
                output.write_u64::<BigEndian>(
                    postings_offset
                        .checked_add(relative_offset)
                        .ok_or_else(|| IndexError::Overflow("directory offset overflow".into()))?,
                )?;
            }
        }
        std::io::copy(self.posting_spool.get_mut(), &mut output)?;
        {
            let checkpoint_reader = self.checkpoint_spool.get_mut();
            for _ in 0..checkpoint_bytes / CHECKPOINT_ENTRY_SIZE {
                let position_index = checkpoint_reader.read_u64::<BigEndian>()?;
                let posting_index = checkpoint_reader.read_u32::<BigEndian>()?;
                let ordinal = checkpoint_reader.read_u32::<BigEndian>()?;
                let relative_offset = checkpoint_reader.read_u64::<BigEndian>()?;
                output.write_u64::<BigEndian>(position_index)?;
                output.write_u32::<BigEndian>(posting_index)?;
                output.write_u32::<BigEndian>(ordinal)?;
                output.write_u64::<BigEndian>(
                    postings_offset
                        .checked_add(relative_offset)
                        .ok_or_else(|| IndexError::Overflow("checkpoint offset overflow".into()))?,
                )?;
            }
        }
        output.sync_all()?;
        drop(output);
        temp_path
            .persist(&self.path)
            .map_err(|error| IndexError::Io(error.error))?;

        Ok(SearchPostingsBuildStats {
            position_count: self.position_count,
            posting_count: self.posting_count,
            game_count: self.game_count,
            file_size_bytes,
            temporary_spool_bytes,
        })
    }
}

/// Memory-mapped reader for a search postings artifact.
pub struct SearchPostingsReader {
    mmap: Mmap,
    position_count: u64,
    posting_count: u64,
    game_count: u64,
    postings_offset: u64,
    checkpoint_offset: u64,
    checkpoint_count: u64,
}

impl SearchPostingsReader {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, IndexError> {
        let file = File::open(path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        Self::from_mmap(mmap)
    }

    pub fn from_mmap(mmap: Mmap) -> Result<Self, IndexError> {
        if mmap.len() < HEADER_LEN {
            return Err(invalid("file is shorter than the postings header"));
        }
        if &mmap[..8] != MAGIC {
            return Err(invalid("invalid postings magic"));
        }
        let version = read_u32(&mmap, 8)?;
        if version != VERSION {
            return Err(invalid(format!("unsupported postings version {version}")));
        }
        if read_u32(&mmap, 12)? as usize != HEADER_LEN {
            return Err(invalid("unexpected postings header size"));
        }
        let position_count = read_u64(&mmap, 16)?;
        let posting_count = read_u64(&mmap, 24)?;
        let game_count = read_u64(&mmap, 32)?;
        let directory_offset = read_u64(&mmap, 40)?;
        let checkpoint_offset = read_u64(&mmap, 48)?;
        let file_size = read_u64(&mmap, 56)?;
        let expected_postings_offset = HEADER_SIZE
            .checked_add(
                position_count
                    .checked_mul(DIRECTORY_ENTRY_SIZE)
                    .ok_or_else(|| invalid("directory size overflow"))?,
            )
            .ok_or_else(|| invalid("postings offset overflow"))?;
        if directory_offset != HEADER_SIZE
            || checkpoint_offset < expected_postings_offset
            || checkpoint_offset > file_size
            || file_size != mmap.len() as u64
            || (file_size - checkpoint_offset).checked_rem(CHECKPOINT_ENTRY_SIZE) != Some(0)
        {
            return Err(invalid("header offsets or file size are inconsistent"));
        }
        if position_count > posting_count || (position_count == 0) != (posting_count == 0) {
            return Err(invalid("position and posting counts are inconsistent"));
        }

        let reader = Self {
            mmap,
            position_count,
            posting_count,
            game_count,
            postings_offset: expected_postings_offset,
            checkpoint_offset,
            checkpoint_count: (file_size - checkpoint_offset) / CHECKPOINT_ENTRY_SIZE,
        };
        reader.verify_directory()?;
        Ok(reader)
    }

    pub fn position_count(&self) -> u64 {
        self.position_count
    }

    pub fn posting_count(&self) -> u64 {
        self.posting_count
    }

    pub fn game_count(&self) -> u64 {
        self.game_count
    }

    /// Finds a position's postings by binary-searching the fixed-width directory.
    pub fn get(&self, key: PositionKey) -> Result<Option<PostingIter<'_>>, IndexError> {
        let mut low = 0u64;
        let mut high = self.position_count;
        while low < high {
            let mid = low + (high - low) / 2;
            let entry = self.directory_entry(mid)?;
            match entry.key.cmp(&key.0) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => {
                    let end = if mid + 1 < self.position_count {
                        self.directory_entry(mid + 1)?.offset
                    } else {
                        self.checkpoint_offset
                    };
                    return Ok(Some(PostingIter::new(
                        &self.mmap,
                        entry.offset,
                        end,
                        self.game_count,
                        key.0,
                        0,
                    )?));
                }
            }
        }
        Ok(None)
    }

    /// Starts iteration at the first posting whose ordinal is at least `ordinal`.
    /// A fixed-stride checkpoint is used when one exists for the selected key.
    pub fn seek(
        &self,
        key: PositionKey,
        ordinal: u32,
    ) -> Result<Option<PostingIter<'_>>, IndexError> {
        let Some(position_index) = self.find_position_index(key)? else {
            return Ok(None);
        };
        let entry = self.directory_entry(position_index)?;
        let end = if position_index + 1 < self.position_count {
            self.directory_entry(position_index + 1)?.offset
        } else {
            self.checkpoint_offset
        };

        let mut low = 0u64;
        let mut high = self.checkpoint_count;
        while low < high {
            let mid = low + (high - low) / 2;
            let checkpoint = self.checkpoint_entry(mid)?;
            if checkpoint.position_index < position_index
                || (checkpoint.position_index == position_index && checkpoint.ordinal < ordinal)
            {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        let checkpoint = if low > 0 {
            let candidate = self.checkpoint_entry(low - 1)?;
            (candidate.position_index == position_index).then_some(candidate)
        } else {
            None
        };
        let (start, first_index) = checkpoint
            .map(|cp| (cp.offset, cp.posting_index))
            .unwrap_or((entry.offset, 0));
        let mut iterator =
            PostingIter::new(&self.mmap, start, end, self.game_count, key.0, first_index)?;
        while let Some(posting) = iterator.next() {
            let posting = posting?;
            if posting.ordinal >= ordinal {
                iterator.pending = Some(posting);
                break;
            }
        }
        Ok(Some(iterator))
    }

    fn find_position_index(&self, key: PositionKey) -> Result<Option<u64>, IndexError> {
        let mut low = 0u64;
        let mut high = self.position_count;
        while low < high {
            let mid = low + (high - low) / 2;
            match self.directory_entry(mid)?.key.cmp(&key.0) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return Ok(Some(mid)),
            }
        }
        Ok(None)
    }

    /// Performs a complete structural scan and reconciles all postings with the header count.
    pub fn verify(&self) -> Result<SearchPostingsBuildStats, IndexError> {
        self.verify_for_coverage(None)
    }

    /// Verifies all entries and optionally enforces a maximum indexed ply.
    pub fn verify_for_coverage(
        &self,
        max_ply: Option<u16>,
    ) -> Result<SearchPostingsBuildStats, IndexError> {
        let mut observed = 0u64;
        let mut checkpoint_cursor = 0u64;
        let mut next_checkpoint = (checkpoint_cursor < self.checkpoint_count)
            .then(|| self.checkpoint_entry(checkpoint_cursor))
            .transpose()?;
        for index in 0..self.position_count {
            let entry = self.directory_entry(index)?;
            let end = if index + 1 < self.position_count {
                self.directory_entry(index + 1)?.offset
            } else {
                self.checkpoint_offset
            };
            let mut iterator =
                PostingIter::new(&self.mmap, entry.offset, end, self.game_count, entry.key, 0)?;
            while let Some(posting) = iterator.next() {
                let posting = posting?;
                if max_ply.is_some_and(|max| posting.ply > max) {
                    return Err(invalid(format!(
                        "posting ply {} exceeds declared coverage {}",
                        posting.ply,
                        max_ply.unwrap()
                    )));
                }
                let posting_index = iterator.last_record_index;
                let anchor = posting_index == 0
                    && next_checkpoint.is_some_and(|checkpoint| {
                        checkpoint.position_index == index && checkpoint.posting_index == 0
                    });
                if anchor || (posting_index > 0 && posting_index.is_multiple_of(CHECKPOINT_STRIDE))
                {
                    let expected = CheckpointEntry {
                        position_index: index,
                        posting_index,
                        ordinal: posting.ordinal,
                        offset: iterator.last_record_offset,
                    };
                    if next_checkpoint != Some(expected) {
                        return Err(invalid("checkpoint does not match its posting"));
                    }
                    checkpoint_cursor += 1;
                    next_checkpoint = (checkpoint_cursor < self.checkpoint_count)
                        .then(|| self.checkpoint_entry(checkpoint_cursor))
                        .transpose()?;
                }
                observed = observed.checked_add(1).ok_or_else(|| {
                    IndexError::Overflow("verified posting count overflow".into())
                })?;
            }
        }
        if checkpoint_cursor != self.checkpoint_count {
            return Err(invalid("checkpoint count does not match posting streams"));
        }
        if observed != self.posting_count {
            return Err(invalid(format!(
                "posting count mismatch: header {}, observed {observed}",
                self.posting_count
            )));
        }
        Ok(SearchPostingsBuildStats {
            position_count: self.position_count,
            posting_count: observed,
            game_count: self.game_count,
            file_size_bytes: self.mmap.len() as u64,
            temporary_spool_bytes: 0,
        })
    }

    fn verify_directory(&self) -> Result<(), IndexError> {
        let mut previous_key = None;
        let mut previous_offset = None;
        for index in 0..self.position_count {
            let entry = self.directory_entry(index)?;
            if previous_key.is_some_and(|key| entry.key <= key) {
                return Err(invalid("directory keys are not strictly increasing"));
            }
            if previous_offset.is_some_and(|offset| entry.offset <= offset) {
                return Err(invalid("directory offsets are not strictly increasing"));
            }
            if entry.offset < self.postings_offset || entry.offset >= self.checkpoint_offset {
                return Err(invalid("directory offset is outside the posting data"));
            }
            previous_key = Some(entry.key);
            previous_offset = Some(entry.offset);
        }
        if self.position_count > 0 && self.directory_entry(0)?.offset != self.postings_offset {
            return Err(invalid(
                "first posting offset does not follow the directory",
            ));
        }
        let mut previous = None;
        for index in 0..self.checkpoint_count {
            let checkpoint = self.checkpoint_entry(index)?;
            if checkpoint.position_index >= self.position_count
                || checkpoint.posting_index.checked_rem(CHECKPOINT_STRIDE) != Some(0)
                || checkpoint.offset < self.postings_offset
                || checkpoint.offset >= self.checkpoint_offset
                || previous.is_some_and(|prior: CheckpointEntry| {
                    (checkpoint.position_index, checkpoint.posting_index)
                        <= (prior.position_index, prior.posting_index)
                })
            {
                return Err(invalid("checkpoint table ordering or bounds are invalid"));
            }
            previous = Some(checkpoint);
        }
        Ok(())
    }

    fn directory_entry(&self, index: u64) -> Result<DirectoryEntry, IndexError> {
        if index >= self.position_count {
            return Err(invalid("directory index is out of bounds"));
        }
        let offset = HEADER_SIZE
            .checked_add(
                index
                    .checked_mul(DIRECTORY_ENTRY_SIZE)
                    .ok_or_else(|| invalid("directory entry offset overflow"))?,
            )
            .ok_or_else(|| invalid("directory entry offset overflow"))?
            as usize;
        let end = offset + DIRECTORY_LEN;
        let bytes = self
            .mmap
            .get(offset..end)
            .ok_or_else(|| invalid("truncated directory entry"))?;
        Ok(DirectoryEntry {
            key: u128::from_be_bytes(bytes[..16].try_into().unwrap()),
            offset: u64::from_be_bytes(bytes[16..24].try_into().unwrap()),
        })
    }

    fn checkpoint_entry(&self, index: u64) -> Result<CheckpointEntry, IndexError> {
        if index >= self.checkpoint_count {
            return Err(invalid("checkpoint index is out of bounds"));
        }
        let offset = self
            .checkpoint_offset
            .checked_add(
                index
                    .checked_mul(CHECKPOINT_ENTRY_SIZE)
                    .ok_or_else(|| invalid("checkpoint entry offset overflow"))?,
            )
            .ok_or_else(|| invalid("checkpoint entry offset overflow"))?;
        let bytes = self
            .mmap
            .get(offset as usize..(offset + CHECKPOINT_ENTRY_SIZE) as usize)
            .ok_or_else(|| invalid("truncated checkpoint entry"))?;
        Ok(CheckpointEntry {
            position_index: u64::from_be_bytes(bytes[0..8].try_into().unwrap()),
            posting_index: u32::from_be_bytes(bytes[8..12].try_into().unwrap()),
            ordinal: u32::from_be_bytes(bytes[12..16].try_into().unwrap()),
            offset: u64::from_be_bytes(bytes[16..24].try_into().unwrap()),
        })
    }
}

/// Iterator over one position's sorted game occurrences.
pub struct PostingIter<'a> {
    mmap: &'a [u8],
    cursor: usize,
    end: usize,
    game_count: u64,
    key: u128,
    previous_ordinal: u32,
    previous_ply: u16,
    record_index: u32,
    last_record_index: u32,
    last_record_offset: u64,
    first: bool,
    failed: bool,
    pending: Option<SearchPosting>,
}

impl<'a> PostingIter<'a> {
    fn new(
        mmap: &'a [u8],
        start: u64,
        end: u64,
        game_count: u64,
        key: u128,
        record_index: u32,
    ) -> Result<Self, IndexError> {
        if start >= end || end > mmap.len() as u64 {
            return Err(invalid("posting stream bounds are invalid"));
        }
        Ok(Self {
            mmap,
            cursor: start as usize,
            end: end as usize,
            game_count,
            key,
            previous_ordinal: 0,
            previous_ply: 0,
            record_index,
            last_record_index: record_index,
            last_record_offset: start,
            first: true,
            failed: false,
            pending: None,
        })
    }
}

impl Iterator for PostingIter<'_> {
    type Item = Result<SearchPosting, IndexError>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(posting) = self.pending.take() {
            return Some(Ok(posting));
        }
        if self.failed || self.cursor == self.end {
            return None;
        }
        let record_offset = self.cursor as u64;
        let record_index = self.record_index;
        let parsed = (|| {
            let ordinal_gap = read_varint(self.mmap, &mut self.cursor, self.end)?;
            let checkpoint_record =
                self.first || record_index.checked_rem(CHECKPOINT_STRIDE) == Some(0);
            let ordinal = if checkpoint_record {
                ordinal_gap
            } else {
                self.previous_ordinal
                    .checked_add(ordinal_gap)
                    .ok_or_else(|| invalid("decoded ordinal overflow"))?
            };
            if u64::from(ordinal) >= self.game_count {
                return Err(invalid(format!(
                    "posting ordinal {ordinal} is outside game_count {}",
                    self.game_count
                )));
            }
            let ply_delta = read_varint(self.mmap, &mut self.cursor, self.end)?;
            let ply = if checkpoint_record || ordinal != self.previous_ordinal {
                u16::try_from(ply_delta).map_err(|_| invalid("decoded ply exceeds u16"))?
            } else {
                self.previous_ply
                    .checked_add(
                        u16::try_from(ply_delta).map_err(|_| invalid("ply delta exceeds u16"))?,
                    )
                    .ok_or_else(|| invalid("decoded ply overflow"))?
            };
            let move_end = self
                .cursor
                .checked_add(2)
                .ok_or_else(|| invalid("move offset overflow"))?;
            let move_bytes = self
                .mmap
                .get(self.cursor..move_end)
                .filter(|_| move_end <= self.end)
                .ok_or_else(|| invalid("truncated move value"))?;
            let encoded_move = u16::from_be_bytes(move_bytes.try_into().unwrap());
            self.cursor = move_end;
            if !self.first
                && (ordinal < self.previous_ordinal
                    || (ordinal == self.previous_ordinal && ply <= self.previous_ply))
            {
                return Err(invalid("decoded ordinal or ply order is invalid"));
            }
            if encoded_move != TERMINAL_MOVE && encoded_move & 0xF000 > 0x4000 {
                return Err(invalid(
                    "decoded compact move has an invalid promotion code",
                ));
            }
            self.previous_ordinal = ordinal;
            self.previous_ply = ply;
            self.first = false;
            self.last_record_index = record_index;
            self.last_record_offset = record_offset;
            self.record_index = self
                .record_index
                .checked_add(1)
                .ok_or_else(|| invalid("posting index overflow"))?;
            Ok(SearchPosting {
                key: PositionKey(self.key),
                ordinal,
                ply,
                played: (encoded_move != TERMINAL_MOVE).then_some(CompactMove(encoded_move)),
            })
        })();
        if parsed.is_err() {
            self.failed = true;
        }
        Some(parsed)
    }
}

fn write_header(
    writer: &mut impl Write,
    position_count: u64,
    posting_count: u64,
    game_count: u64,
    postings_offset: u64,
    file_size: u64,
) -> Result<(), IndexError> {
    writer.write_all(MAGIC)?;
    writer.write_u32::<BigEndian>(VERSION)?;
    writer.write_u32::<BigEndian>(HEADER_SIZE as u32)?;
    writer.write_u64::<BigEndian>(position_count)?;
    writer.write_u64::<BigEndian>(posting_count)?;
    writer.write_u64::<BigEndian>(game_count)?;
    writer.write_u64::<BigEndian>(HEADER_SIZE)?;
    writer.write_u64::<BigEndian>(postings_offset)?;
    writer.write_u64::<BigEndian>(file_size)?;
    Ok(())
}

fn write_varint(writer: &mut impl Write, mut value: u32) -> Result<(), std::io::Error> {
    while value >= 0x80 {
        writer.write_all(&[((value as u8) & 0x7f) | 0x80])?;
        value >>= 7;
    }
    writer.write_all(&[value as u8])
}

fn varint_len(value: u32) -> usize {
    if value < (1 << 7) {
        1
    } else if value < (1 << 14) {
        2
    } else if value < (1 << 21) {
        3
    } else if value < (1 << 28) {
        4
    } else {
        5
    }
}

fn read_varint(bytes: &[u8], cursor: &mut usize, end: usize) -> Result<u32, IndexError> {
    let start = *cursor;
    let mut value = 0u32;
    for index in 0..MAX_VARINT_BYTES {
        let byte = *bytes
            .get(*cursor)
            .filter(|_| *cursor < end)
            .ok_or_else(|| invalid("truncated variable integer"))?;
        *cursor += 1;
        if index == 4 && byte & 0xf0 != 0 {
            return Err(invalid("variable integer overflows u32"));
        }
        value |= u32::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            let encoded_len = *cursor - start;
            let minimal_len = if value < (1 << 7) {
                1
            } else if value < (1 << 14) {
                2
            } else if value < (1 << 21) {
                3
            } else if value < (1 << 28) {
                4
            } else {
                5
            };
            if encoded_len != minimal_len {
                return Err(invalid("non-canonical variable integer"));
            }
            return Ok(value);
        }
    }
    Err(invalid("variable integer exceeds five bytes"))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, IndexError> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| invalid("truncated header field"))?;
    Ok(u32::from_be_bytes(value.try_into().unwrap()))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, IndexError> {
    let value = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| invalid("truncated header field"))?;
    Ok(u64::from_be_bytes(value.try_into().unwrap()))
}

fn invalid(message: impl Into<String>) -> IndexError {
    IndexError::InvalidFormat(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn posting(key: u128, ordinal: u32, ply: u16, played: Option<CompactMove>) -> SearchPosting {
        SearchPosting {
            key: PositionKey(key),
            ordinal,
            ply,
            played,
        }
    }

    fn records() -> Vec<Result<SearchPosting, IndexError>> {
        vec![
            Ok(posting(3, 0, 0, Some(CompactMove::new(12, 28, None)))),
            Ok(posting(3, 0, 4, Some(CompactMove::new(12, 28, None)))),
            Ok(posting(3, 2, 1, None)),
            Ok(posting(9, 1, 0, None)),
        ]
    }

    #[test]
    fn writes_deterministically_and_supports_binary_search_and_iteration() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.postings");
        let second = dir.path().join("second.postings");
        let stats_a = write_search_postings(&first, 3, records()).unwrap();
        let stats_b = write_search_postings(&second, 3, records()).unwrap();
        assert_eq!(stats_a, stats_b);
        assert_eq!(fs::read(first).unwrap(), fs::read(&second).unwrap());

        let reader = SearchPostingsReader::open(second).unwrap();
        let verified = reader.verify().unwrap();
        assert_eq!(verified.position_count, stats_a.position_count);
        assert_eq!(verified.posting_count, stats_a.posting_count);
        assert_eq!(verified.file_size_bytes, stats_a.file_size_bytes);
        assert_eq!(reader.position_count(), 2);
        assert_eq!(reader.posting_count(), 4);
        let found = reader
            .get(PositionKey(3))
            .unwrap()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            found,
            vec![
                posting(3, 0, 0, Some(CompactMove::new(12, 28, None))),
                posting(3, 0, 4, Some(CompactMove::new(12, 28, None))),
                posting(3, 2, 1, None),
            ]
        );
        assert!(reader.get(PositionKey(8)).unwrap().is_none());
    }

    #[test]
    fn accepts_empty_artifact_and_terminal_occurrences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.postings");
        let stats = write_search_postings(&path, 0, std::iter::empty()).unwrap();
        assert_eq!(stats.position_count, 0);
        assert_eq!(stats.posting_count, 0);
        let verified = SearchPostingsReader::open(path).unwrap().verify().unwrap();
        assert_eq!(verified.position_count, stats.position_count);
        assert_eq!(verified.posting_count, stats.posting_count);
    }

    #[test]
    fn rejects_unsorted_duplicate_and_out_of_range_input() {
        let dir = tempfile::tempdir().unwrap();
        let cases = [
            vec![Ok(posting(2, 0, 0, None)), Ok(posting(1, 0, 1, None))],
            vec![Ok(posting(2, 0, 0, None)), Ok(posting(2, 0, 0, None))],
            vec![Ok(posting(2, 3, 0, None))],
        ];
        for (index, records) in cases.into_iter().enumerate() {
            let path = dir.path().join(format!("bad-{index}.postings"));
            assert!(write_search_postings(path, 3, records).is_err());
        }
    }

    #[test]
    fn rejects_corrupt_offsets_and_truncated_postings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.postings");
        write_search_postings(&path, 3, records()).unwrap();

        let mut corrupt = fs::read(&path).unwrap();
        corrupt[64 + 16..64 + 24].copy_from_slice(&u64::MAX.to_be_bytes());
        let bad_offset = dir.path().join("bad-offset.postings");
        fs::write(&bad_offset, corrupt).unwrap();
        assert!(SearchPostingsReader::open(bad_offset).is_err());

        let truncated = dir.path().join("truncated.postings");
        let mut bytes = fs::read(path).unwrap();
        bytes.pop();
        let reported_len = bytes.len() as u64;
        bytes[56..64].copy_from_slice(&reported_len.to_be_bytes());
        fs::write(&truncated, bytes).unwrap();
        assert!(SearchPostingsReader::open(truncated).is_err());
    }

    #[test]
    fn rejects_noncanonical_or_overflowing_varints() {
        let mut cursor = 0;
        assert!(read_varint(&[0x80, 0x00], &mut cursor, 2).is_err());
        cursor = 0;
        assert!(read_varint(&[0xff, 0xff, 0xff, 0xff, 0x10], &mut cursor, 5).is_err());
        cursor = 0;
        assert_eq!(read_varint(&[0xac, 0x02], &mut cursor, 2).unwrap(), 300);
    }

    #[test]
    fn seeks_keep_the_earliest_matching_ply_when_a_repetition_crosses_a_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repeated.postings");
        let records = (0..255)
            .map(|ordinal| Ok(posting(7, ordinal, 0, None)))
            .chain([Ok(posting(7, 255, 5, None)), Ok(posting(7, 255, 9, None))]);
        write_search_postings(&path, 256, records).unwrap();
        let index = SearchPostingsReader::open(&path).unwrap();
        index.verify().unwrap();
        assert_eq!(index.checkpoint_count, 1);
        let positions = index
            .seek(PositionKey(7), 255)
            .unwrap()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            positions
                .iter()
                .map(|position| position.ply)
                .collect::<Vec<_>>(),
            [5, 9]
        );
    }

    #[test]
    fn seeks_from_fixed_stride_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("checkpointed.postings");
        let records = (0..600)
            .map(|ordinal| Ok(posting(7, ordinal, 0, Some(CompactMove::new(12, 28, None)))))
            .collect::<Vec<_>>();
        write_search_postings(&path, 600, records).unwrap();
        let reader = SearchPostingsReader::open(path).unwrap();
        assert_eq!(reader.verify().unwrap().posting_count, 600);
        let found = reader
            .seek(PositionKey(7), 400)
            .unwrap()
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(found.ordinal, 400);
        assert!(reader
            .seek(PositionKey(7), 600)
            .unwrap()
            .unwrap()
            .next()
            .is_none());
    }
}
