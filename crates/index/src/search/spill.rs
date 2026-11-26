//! Bounded external sorting of position occurrences before encoding postings.

use crate::position::IndexError;
use crate::search::PositionOccurrence;
use chess_database_core::{CompactMove, PositionKey};
use sha2::{Digest, Sha256};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

const RECORD_BYTES: usize = 24;
const CHECKSUM_BYTES: usize = 32;
const FAN_IN: usize = 16;
const IO_BUFFER_BYTES: usize = 64 * 1024;
const TERMINAL: u16 = u16::MAX;

type Record = [u8; RECORD_BYTES];

fn encode(game: u32, occurrence: PositionOccurrence) -> Record {
    let mut bytes = [0; RECORD_BYTES];
    bytes[..16].copy_from_slice(&occurrence.key.0.to_be_bytes());
    bytes[16..20].copy_from_slice(&game.to_be_bytes());
    bytes[20..22].copy_from_slice(&occurrence.ply.to_be_bytes());
    bytes[22..24].copy_from_slice(&occurrence.played.map_or(TERMINAL, |m| m.0).to_be_bytes());
    bytes
}

fn decode(record: Record) -> (PositionKey, u32, u16, Option<CompactMove>) {
    let played = u16::from_be_bytes(record[22..24].try_into().unwrap());
    (
        PositionKey(u128::from_be_bytes(record[..16].try_into().unwrap())),
        u32::from_be_bytes(record[16..20].try_into().unwrap()),
        u16::from_be_bytes(record[20..22].try_into().unwrap()),
        (played != TERMINAL).then_some(CompactMove(played)),
    )
}

struct Run {
    path: PathBuf,
    records: u64,
}

impl Run {
    fn bytes(&self) -> u64 {
        self.records * RECORD_BYTES as u64 + CHECKSUM_BYTES as u64
    }
}

struct RunReader {
    file: BufReader<File>,
    remaining: u64,
    digest: Sha256,
}

impl RunReader {
    fn open(run: &Run) -> io::Result<Self> {
        let file = File::open(&run.path)?;
        if file.metadata()?.len() != run.bytes() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Search spill length mismatch",
            ));
        }
        Ok(Self {
            file: BufReader::with_capacity(IO_BUFFER_BYTES, file),
            remaining: run.records,
            digest: Sha256::new(),
        })
    }

    fn next_record(&mut self) -> io::Result<Option<Record>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let mut bytes = [0; RECORD_BYTES];
        self.file.read_exact(&mut bytes)?;
        self.digest.update(bytes);
        self.remaining -= 1;
        if self.remaining == 0 {
            let mut expected = [0; CHECKSUM_BYTES];
            self.file.read_exact(&mut expected)?;
            let actual: [u8; CHECKSUM_BYTES] = self.digest.finalize_reset().into();
            if actual != expected {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Search spill checksum mismatch",
                ));
            }
        }
        Ok(Some(bytes))
    }
}

struct RunWriter {
    file: BufWriter<File>,
    digest: Sha256,
}

impl RunWriter {
    fn create(path: &Path) -> io::Result<Self> {
        Ok(Self {
            file: BufWriter::with_capacity(IO_BUFFER_BYTES, File::create_new(path)?),
            digest: Sha256::new(),
        })
    }

    fn write(&mut self, record: Record) -> io::Result<()> {
        self.digest.update(record);
        self.file.write_all(&record)
    }

    fn finish(mut self) -> io::Result<()> {
        self.file.write_all(&self.digest.finalize())?;
        self.file.flush()
    }
}

fn merge(
    runs: &[Run],
    mut emit: impl FnMut(Record) -> Result<(), IndexError>,
) -> Result<(), IndexError> {
    let mut readers = Vec::with_capacity(runs.len());
    let mut heap = BinaryHeap::with_capacity(runs.len());
    for (index, run) in runs.iter().enumerate() {
        let mut reader = RunReader::open(run)?;
        if let Some(record) = reader.next_record()? {
            heap.push(Reverse((record, index)));
        }
        readers.push(reader);
    }
    let mut previous = None;
    while let Some(Reverse((record, index))) = heap.pop() {
        if previous.is_some_and(|last| last >= record) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Search spill contains duplicate or unsorted records",
            )
            .into());
        }
        emit(record)?;
        previous = Some(record);
        if let Some(next) = readers[index].next_record()? {
            heap.push(Reverse((next, index)));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct SpillStats {
    pub buffer_bytes: usize,
    pub initial_runs: u64,
    pub intermediate_merges: u64,
    pub peak_temp_bytes: u64,
}

/// Sorts by (canonical position, ordinal, ply, continuation). Sorted runs are compacted
/// periodically to bound the number of open files and retained run descriptors.
/// The buffer limit covers in-memory sort records. Merge readers use up to 1 MiB additionally.
pub struct OccurrenceSorter {
    workspace: tempfile::TempDir,
    buffer: Vec<Record>,
    levels: Vec<Vec<Run>>,
    records_per_run: usize,
    next_run: u64,
    live_bytes: u64,
    stats: SpillStats,
}

impl OccurrenceSorter {
    pub fn new(temp_dir: &Path, buffer_limit_bytes: usize) -> Result<Self, IndexError> {
        let records_per_run = buffer_limit_bytes / RECORD_BYTES;
        if records_per_run == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Search sort buffer must fit at least one record",
            )
            .into());
        }
        let mut buffer = Vec::new();
        buffer.try_reserve_exact(records_per_run).map_err(|err| {
            io::Error::new(
                io::ErrorKind::OutOfMemory,
                format!("Cannot allocate search sort buffer: {err}"),
            )
        })?;
        Ok(Self {
            workspace: tempfile::Builder::new()
                .prefix("search-runs-")
                .tempdir_in(temp_dir)?,
            buffer,
            levels: Vec::new(),
            records_per_run,
            next_run: 0,
            live_bytes: 0,
            stats: SpillStats {
                buffer_bytes: records_per_run * RECORD_BYTES,
                ..SpillStats::default()
            },
        })
    }

    pub fn push(&mut self, game: u32, occurrence: PositionOccurrence) -> Result<(), IndexError> {
        self.buffer.push(encode(game, occurrence));
        if self.buffer.len() == self.records_per_run {
            self.spill()?;
        }
        Ok(())
    }

    fn run(&mut self, records: u64) -> Run {
        let path = self
            .workspace
            .path()
            .join(format!("{:016x}.run", self.next_run));
        self.next_run += 1;
        Run { path, records }
    }

    fn record_disk_bytes(&mut self, run: &Run) {
        self.live_bytes += run.bytes();
        self.stats.peak_temp_bytes = self.stats.peak_temp_bytes.max(self.live_bytes);
    }

    fn merge_to_run(&mut self, runs: Vec<Run>) -> Result<Run, IndexError> {
        let result = self.run(runs.iter().map(|run| run.records).sum());
        let mut writer = RunWriter::create(&result.path)?;
        merge(&runs, |record| writer.write(record).map_err(Into::into))?;
        writer.finish()?;
        self.record_disk_bytes(&result);
        for run in runs {
            fs::remove_file(&run.path)?;
            self.live_bytes -= run.bytes();
        }
        self.stats.intermediate_merges += 1;
        Ok(result)
    }

    fn spill(&mut self) -> Result<(), IndexError> {
        self.buffer.sort_unstable();
        let mut run = self.run(self.buffer.len() as u64);
        let mut writer = RunWriter::create(&run.path)?;
        for record in self.buffer.drain(..) {
            writer.write(record)?;
        }
        writer.finish()?;
        self.record_disk_bytes(&run);
        self.stats.initial_runs += 1;
        let mut level = 0;
        loop {
            if level == self.levels.len() {
                self.levels.push(Vec::new());
            }
            self.levels[level].push(run);
            if self.levels[level].len() < FAN_IN {
                return Ok(());
            }
            let runs = std::mem::take(&mut self.levels[level]);
            run = self.merge_to_run(runs)?;
            level += 1;
        }
    }

    pub fn finish(
        mut self,
        mut emit: impl FnMut(PositionKey, u32, u16, Option<CompactMove>) -> Result<(), IndexError>,
    ) -> Result<SpillStats, IndexError> {
        if self.stats.initial_runs == 0 {
            self.buffer.sort_unstable();
            if self.buffer.windows(2).any(|pair| pair[0] == pair[1]) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Search sort contains duplicate records",
                )
                .into());
            }
            for record in self.buffer {
                let (key, game, ply, played) = decode(record);
                emit(key, game, ply, played)?;
            }
            return Ok(self.stats);
        }

        if !self.buffer.is_empty() {
            self.spill()?;
        }
        self.buffer = Vec::new();
        let mut runs: Vec<_> = self.levels.drain(..).rev().flatten().collect();
        while runs.len() > FAN_IN {
            let tail = runs.split_off(runs.len() - FAN_IN);
            let combined = self.merge_to_run(tail)?;
            runs.insert(0, combined);
        }
        merge(&runs, |record| {
            let (key, game, ply, played) = decode(record);
            emit(key, game, ply, played)
        })?;
        Ok(self.stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_deterministic_across_spill_budgets() {
        let dir = tempfile::tempdir().unwrap();
        let produce = |buffer| {
            let mut sorter = OccurrenceSorter::new(dir.path(), buffer).unwrap();
            for game in (0..70).rev() {
                for ply in (0..4).rev() {
                    sorter
                        .push(
                            game,
                            PositionOccurrence {
                                key: PositionKey(u128::from(ply % 2)),
                                ply,
                                played: Some(CompactMove(ply)),
                            },
                        )
                        .unwrap();
                }
            }
            let mut values = Vec::new();
            let stats = sorter
                .finish(|k, g, p, m| {
                    values.push((k.0, g, p, m));
                    Ok(())
                })
                .unwrap();
            (values, stats)
        };
        let (in_memory, no_spill) = produce(512 * RECORD_BYTES);
        let (merged, spilled) = produce(2 * RECORD_BYTES);
        assert_eq!(in_memory.len(), 280);
        assert!(in_memory.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(in_memory, merged);
        assert_eq!(no_spill.peak_temp_bytes, 0);
        assert!(spilled.intermediate_merges > 1);
        assert!(spilled.peak_temp_bytes >= in_memory.len() as u64 * RECORD_BYTES as u64);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn checksum_failure_removes_scratch_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut sorter = OccurrenceSorter::new(dir.path(), RECORD_BYTES).unwrap();
        sorter
            .push(
                0,
                PositionOccurrence {
                    key: PositionKey(5),
                    ply: 0,
                    played: None,
                },
            )
            .unwrap();
        let run = &sorter.levels[0][0];
        let mut bytes = fs::read(&run.path).unwrap();
        bytes[0] ^= 1;
        fs::write(&run.path, bytes).unwrap();
        assert!(sorter.finish(|_, _, _, _| Ok(())).is_err());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
