//! Deterministic, streaming PGN export of a persistent dataset (`export-pgn`).
//!
//! The export walks the immutable snapshot once in physical order
//! (`DatasetReader::iter_games`) and writes every canonical game exactly once.
//! Its output is a pure function of the snapshot and of this format, so two runs
//! over the same snapshot produce byte-identical PGN.
//!
//! Each game is rebuilt from two stored sources:
//!
//! * the CGB3 block: player names, Elo, date, event, site, round, result, ECO, time
//!   control, `GameURL` (source ID), `BroadcastURL`, the FEN of a non-standard start,
//!   and the complete mainline;
//! * the attribution sidecar named by the manifest (`source-attribution.jsonl`),
//!   joined by stable game ID to add `Source`, `WhiteTitle`/`BlackTitle`,
//!   `WhiteFideId`/`BlackFideId`, and `Mode`.
//!
//! Stored data cannot reproduce comments, NAGs, variations, clock annotations,
//! `Termination`, identity-evidence labels, or source-quality labels, so none are
//! written. A sidecar value that disagrees across the source attributions merged into
//! one game is omitted and counted in the report instead of being guessed. The sidecar
//! is project curation metadata, not a copy of each source record, so the `Source`
//! tag lists every publisher tag attributed to the game.
//!
//! Fail-closed rules: an illegal stored move, a FEN that does not describe a legal
//! standard position, a control character in a tag value, a missing or duplicate
//! sidecar entry, or a game count that differs from the manifest aborts the export
//! and leaves no output file behind.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use opening_explorer_core::{CompactMove, ParsedGame};
use opening_explorer_storage::{compute_file_sha256, DatasetReader};
use serde::{Deserialize, Serialize};
use shakmaty::fen::Fen;
use shakmaty::san::SanPlus;
use shakmaty::{CastlingMode, Chess, Color, Position};

/// Zstandard level for `.zst` output. Chosen for the distributed artifact, not for speed.
pub const DEFAULT_ZSTD_LEVEL: i32 = 19;
/// Movetext lines are wrapped before this column, following the common PGN convention.
const MOVETEXT_WIDTH: usize = 80;
const SIDECAR_SCHEMA_VERSION: u32 = 1;
const REPORT_SCHEMA_VERSION: u32 = 1;
const UNKNOWN_TEXT: &str = "?";
const UNKNOWN_DATE: &str = "????.??.??";

/// Options for one export run. `output: None` writes to stdout.
#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub dataset: PathBuf,
    pub output: Option<PathBuf>,
    pub zstd_level: i32,
    pub report: Option<PathBuf>,
}

/// Counters that explain every deviation from a plain copy of the stored fields.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct ExportStats {
    pub games_written: u64,
    pub plain_bytes: u64,
    pub setup_games: u64,
    pub missing_source_tag: u64,
    /// Count of games whose sidecar values disagreed, per omitted PGN tag.
    pub omitted_conflicting_tags: std::collections::BTreeMap<String, u64>,
}

impl ExportStats {
    fn omit_conflict(&mut self, tag: &str) {
        *self
            .omitted_conflicting_tags
            .entry(tag.to_string())
            .or_default() += 1;
    }
}

#[derive(Debug, Deserialize)]
struct AttributionLine {
    schema_version: u32,
    game_id: u64,
    source_attributions: Vec<AttributionRecord>,
}

#[derive(Debug, Deserialize)]
struct AttributionRecord {
    source_tag: Option<String>,
    white_fide_id: Option<String>,
    black_fide_id: Option<String>,
    mode: Option<String>,
    white_title: Option<String>,
    black_title: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct IndexEntry {
    game_id: u64,
    start: u64,
    len: u32,
}

/// Sorted in-memory map from stable game ID to the byte range of its sidecar line.
/// It holds 24 bytes per game (about 79 MiB for the 3,284,429-game Masters release);
/// sidecar lines are read on demand with positioned reads, not loaded in bulk.
struct AttributionIndex {
    file: File,
    entries: Vec<IndexEntry>,
    used: Vec<bool>,
}

impl AttributionIndex {
    fn open(path: &Path, expected_entries: u64) -> Result<Self> {
        let mut scan = BufReader::with_capacity(1 << 20, File::open(path)?);
        let file = File::open(path)?;
        let mut entries: Vec<IndexEntry> = Vec::new();
        let mut offset: u64 = 0;
        let mut line = Vec::with_capacity(1024);
        loop {
            line.clear();
            let read = scan.read_until(b'\n', &mut line)?;
            if read == 0 {
                break;
            }
            let record = parse_attribution_line(&line)
                .with_context(|| format!("invalid source-attribution line at byte {offset}"))?;
            entries.push(IndexEntry {
                game_id: record.game_id,
                start: offset,
                len: u32::try_from(read).context("source-attribution line exceeds 4 GiB")?,
            });
            offset += read as u64;
        }
        if entries.len() as u64 != expected_entries {
            bail!(
                "source-attribution sidecar has {} entries, manifest declares {}",
                entries.len(),
                expected_entries
            );
        }
        entries.sort_unstable_by_key(|entry| entry.game_id);
        if entries
            .windows(2)
            .any(|pair| pair[0].game_id == pair[1].game_id)
        {
            bail!("source-attribution sidecar contains a duplicate game_id");
        }
        let used = vec![false; entries.len()];
        Ok(AttributionIndex {
            file,
            entries,
            used,
        })
    }

    /// Returns the attribution line for `game_id`, marking it as consumed. Each sidecar
    /// entry may be consumed once, so a duplicated canonical game ID is rejected.
    fn take(&mut self, game_id: u64) -> Result<AttributionLine> {
        let index = self
            .entries
            .binary_search_by_key(&game_id, |entry| entry.game_id)
            .map_err(|_| anyhow!("no source attribution for game {game_id}"))?;
        if std::mem::replace(&mut self.used[index], true) {
            bail!("game {game_id} was exported more than once");
        }
        let entry = self.entries[index];
        let mut buffer = vec![0u8; entry.len as usize];
        self.file
            .read_exact_at(&mut buffer, entry.start)
            .with_context(|| format!("reading source attribution for game {game_id}"))?;
        let record = parse_attribution_line(&buffer)?;
        if record.game_id != game_id {
            bail!(
                "source attribution for game {game_id} points at game {}",
                record.game_id
            );
        }
        Ok(record)
    }

    fn unconsumed(&self) -> usize {
        self.used.iter().filter(|consumed| !**consumed).count()
    }
}

fn parse_attribution_line(bytes: &[u8]) -> Result<AttributionLine> {
    let record: AttributionLine =
        serde_json::from_slice(bytes).context("malformed source-attribution JSON")?;
    if record.schema_version != SIDECAR_SCHEMA_VERSION {
        bail!(
            "unsupported source-attribution schema version {}",
            record.schema_version
        );
    }
    Ok(record)
}

/// Runs the export and returns its counters. Refuses to overwrite an existing output.
pub fn run_export_pgn(options: &ExportOptions) -> Result<ExportStats> {
    if let Some(report) = &options.report {
        ensure_absent(report)?;
    }
    let reader = DatasetReader::open(&options.dataset)
        .with_context(|| format!("failed to open dataset {}", options.dataset.display()))?;
    let manifest = reader.manifest().clone();
    let sidecar_meta = manifest.source_attribution.clone().ok_or_else(|| {
        anyhow!("dataset manifest has no source-attribution sidecar; export-pgn requires one")
    })?;
    let sidecar = reader.snapshot_dir().join(&sidecar_meta.filename);
    let size = fs::metadata(&sidecar)
        .with_context(|| format!("missing source-attribution sidecar {}", sidecar.display()))?
        .len();
    if size != sidecar_meta.file_size_bytes {
        bail!("source-attribution sidecar size differs from the manifest");
    }
    if compute_file_sha256(&sidecar)? != sidecar_meta.sha256 {
        bail!("source-attribution sidecar SHA-256 differs from the manifest");
    }
    let mut index = AttributionIndex::open(&sidecar, manifest.total_games)?;

    let stats = match &options.output {
        None => {
            let stdout = std::io::stdout();
            let mut writer = BufWriter::with_capacity(1 << 20, stdout.lock());
            let stats = export_games(&reader, &mut index, &mut writer)?;
            writer.flush()?;
            stats
        }
        Some(path) => {
            ensure_absent(path)?;
            write_atomic(path, options.zstd_level, |sink| {
                export_games(&reader, &mut index, sink)
            })?
        }
    };

    if stats.games_written != manifest.total_games {
        bail!(
            "exported {} games, manifest declares {}",
            stats.games_written,
            manifest.total_games
        );
    }
    if index.unconsumed() != 0 {
        bail!(
            "{} source-attribution entries were not consumed",
            index.unconsumed()
        );
    }

    if let Some(report_path) = &options.report {
        let report = ExportReport {
            schema_version: REPORT_SCHEMA_VERSION,
            dataset_build_id: &manifest.dataset_build_id,
            collection: manifest.collection.to_string(),
            expected_games: manifest.total_games,
            output_file: options
                .output
                .as_deref()
                .and_then(Path::file_name)
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "stdout".to_string()),
            zstd_level: options
                .output
                .as_deref()
                .filter(|path| is_zst(path))
                .map(|_| options.zstd_level),
            source_attribution: AttributionReport {
                filename: sidecar_meta.filename.clone(),
                sha256: sidecar_meta.sha256.clone(),
                entries: manifest.total_games,
            },
            stats: &stats,
        };
        let mut json = serde_json::to_string_pretty(&report)?;
        json.push('\n');
        ensure_absent(report_path)?;
        fs::write(report_path, json)
            .with_context(|| format!("writing report {}", report_path.display()))?;
    }
    Ok(stats)
}

#[derive(Serialize)]
struct ExportReport<'a> {
    schema_version: u32,
    dataset_build_id: &'a str,
    collection: String,
    expected_games: u64,
    output_file: String,
    zstd_level: Option<i32>,
    source_attribution: AttributionReport,
    #[serde(flatten)]
    stats: &'a ExportStats,
}

#[derive(Serialize)]
struct AttributionReport {
    filename: String,
    sha256: String,
    entries: u64,
}

fn export_games(
    reader: &DatasetReader,
    index: &mut AttributionIndex,
    sink: &mut dyn Write,
) -> Result<ExportStats> {
    let mut stats = ExportStats::default();
    let mut text = String::with_capacity(4096);
    for game in reader
        .iter_games()
        .context("opening canonical game iterator")?
    {
        let game = game.context("reading canonical game")?;
        let attributions = index.take(game.metadata.id)?;
        text.clear();
        render_game(&game, &attributions, &mut stats, &mut text)?;
        sink.write_all(text.as_bytes())?;
        stats.plain_bytes += text.len() as u64;
        stats.games_written += 1;
    }
    Ok(stats)
}

fn is_zst(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("zst")
}

fn ensure_absent(path: &Path) -> Result<()> {
    if path.exists() {
        bail!(
            "refusing to overwrite existing file {}; remove it first",
            path.display()
        );
    }
    Ok(())
}

/// Writes through a `.partial` sibling and renames it only after a complete,
/// flushed (and, for `.zst`, checksummed) stream, so a failed run never leaves a
/// plausible-looking output file.
fn write_atomic<T>(
    path: &Path,
    zstd_level: i32,
    produce: impl FnOnce(&mut dyn Write) -> Result<T>,
) -> Result<T> {
    let parent = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("output path {} has no file name", path.display()))?;
    let partial = parent.join(format!(
        ".{}.partial-{}",
        name.to_string_lossy(),
        std::process::id()
    ));
    let outcome = (|| -> Result<T> {
        let file =
            File::create(&partial).with_context(|| format!("creating {}", partial.display()))?;
        let mut buffered = BufWriter::with_capacity(1 << 20, file);
        let value = if is_zst(path) {
            let mut encoder = zstd::Encoder::new(buffered, zstd_level)?;
            encoder.include_checksum(true)?;
            let value = produce(&mut encoder)?;
            buffered = encoder.finish()?;
            value
        } else {
            produce(&mut buffered)?
        };
        buffered.flush()?;
        let file = buffered.into_inner().map_err(|error| error.into_error())?;
        file.sync_all()?;
        Ok(value)
    })();
    match outcome {
        Ok(value) => {
            fs::rename(&partial, path).with_context(|| format!("publishing {}", path.display()))?;
            Ok(value)
        }
        Err(error) => {
            let _ = fs::remove_file(&partial);
            Err(error)
        }
    }
}

/// Renders one game (tag section, blank line, movetext, blank line) into `out`.
fn render_game(
    game: &ParsedGame,
    attributions: &AttributionLine,
    stats: &mut ExportStats,
    out: &mut String,
) -> Result<()> {
    let start = start_position(game)?;
    let tags = build_tags(game, attributions, stats)?;
    for (name, value) in &tags {
        push_tag(out, name, value, game.metadata.id)?;
    }
    out.push('\n');
    write_movetext(out, start, game)?;
    out.push('\n');
    Ok(())
}

fn start_position(game: &ParsedGame) -> Result<Chess> {
    match game.fen_start.as_deref() {
        None => Ok(Chess::default()),
        Some(fen) => {
            let parsed = Fen::from_ascii(fen.as_bytes()).map_err(|error| {
                anyhow!("game {}: invalid FEN start: {error}", game.metadata.id)
            })?;
            parsed
                .into_position::<Chess>(CastlingMode::Standard)
                .map_err(|error| {
                    anyhow!(
                        "game {}: FEN start is not a legal standard position: {error}",
                        game.metadata.id
                    )
                })
        }
    }
}

fn build_tags(
    game: &ParsedGame,
    attributions: &AttributionLine,
    stats: &mut ExportStats,
) -> Result<Vec<(&'static str, String)>> {
    let meta = &game.metadata;
    let records = &attributions.source_attributions;
    let mut tags: Vec<(&'static str, String)> = Vec::with_capacity(24);

    tags.push(("Event", or_unknown(meta.event.as_deref())));
    tags.push(("Site", or_unknown(meta.site.as_deref())));
    tags.push((
        "Date",
        meta.date
            .clone()
            .unwrap_or_else(|| UNKNOWN_DATE.to_string()),
    ));
    tags.push(("Round", or_unknown(meta.round.as_deref())));
    tags.push(("White", or_unknown(Some(meta.white.as_str()))));
    tags.push(("Black", or_unknown(Some(meta.black.as_str()))));
    tags.push(("Result", meta.result.to_str().to_string()));

    attribute_single(
        &mut tags,
        stats,
        "WhiteTitle",
        distinct(records.iter().map(|r| r.white_title.as_deref())),
    );
    attribute_single(
        &mut tags,
        stats,
        "BlackTitle",
        distinct(records.iter().map(|r| r.black_title.as_deref())),
    );
    if let Some(elo) = meta.white_elo {
        tags.push(("WhiteElo", elo.to_string()));
    }
    if let Some(elo) = meta.black_elo {
        tags.push(("BlackElo", elo.to_string()));
    }
    attribute_single(
        &mut tags,
        stats,
        "WhiteFideId",
        distinct(records.iter().map(|r| r.white_fide_id.as_deref())),
    );
    attribute_single(
        &mut tags,
        stats,
        "BlackFideId",
        distinct(records.iter().map(|r| r.black_fide_id.as_deref())),
    );
    if let Some(eco) = meta.eco.as_deref().filter(|v| !v.is_empty()) {
        tags.push(("ECO", eco.to_string()));
    }
    if let Some(time_control) = meta.time_control.as_deref().filter(|v| !v.is_empty()) {
        tags.push(("TimeControl", time_control.to_string()));
    }
    tags.push(("PlyCount", game.moves.len().to_string()));

    let sources: Vec<&str> = distinct(records.iter().map(|r| r.source_tag.as_deref()))
        .into_iter()
        .collect();
    if sources.is_empty() {
        stats.missing_source_tag += 1;
    } else {
        tags.push(("Source", sources.join("; ")));
    }
    attribute_single(
        &mut tags,
        stats,
        "Mode",
        distinct(records.iter().map(|r| r.mode.as_deref())),
    );

    if let Some(source_id) = meta.source_id.as_deref().filter(|v| !v.is_empty()) {
        tags.push(("GameURL", source_id.to_string()));
    }
    if let Some(broadcast) = meta.broadcast_url.as_deref().filter(|v| !v.is_empty()) {
        tags.push(("BroadcastURL", broadcast.to_string()));
    }
    if let Some(fen) = game.fen_start.as_deref() {
        tags.push(("SetUp", "1".to_string()));
        tags.push(("FEN", fen.to_string()));
        stats.setup_games += 1;
    }
    Ok(tags)
}

/// Adds a tag only when every attribution agrees on one non-empty value.
fn attribute_single(
    tags: &mut Vec<(&'static str, String)>,
    stats: &mut ExportStats,
    tag: &'static str,
    values: BTreeSet<&str>,
) {
    match values.len() {
        0 => {}
        1 => tags.push((tag, values.first().copied().unwrap_or_default().to_string())),
        _ => stats.omit_conflict(tag),
    }
}

fn distinct<'a>(values: impl Iterator<Item = Option<&'a str>>) -> BTreeSet<&'a str> {
    values.flatten().filter(|value| !value.is_empty()).collect()
}

fn or_unknown(value: Option<&str>) -> String {
    match value {
        Some(text) if !text.is_empty() => text.to_string(),
        _ => UNKNOWN_TEXT.to_string(),
    }
}

/// Writes one tag pair with PGN string escaping. Control characters cannot be
/// represented in a tag value without breaking the file, so they fail closed.
fn push_tag(out: &mut String, name: &str, value: &str, game_id: u64) -> Result<()> {
    if value.chars().any(char::is_control) {
        bail!("game {game_id}: tag {name} contains a control character");
    }
    out.push('[');
    out.push_str(name);
    out.push_str(" \"");
    for character in value.chars() {
        if character == '"' || character == '\\' {
            out.push('\\');
        }
        out.push(character);
    }
    out.push_str("\"]\n");
    Ok(())
}

/// Replays the stored mainline from the start position and writes SAN with check and
/// mate markers, numbering moves as PGN requires, then the stored result.
fn write_movetext(out: &mut String, mut board: Chess, game: &ParsedGame) -> Result<()> {
    let game_id = game.metadata.id;
    let mut wrapper = MovetextWrapper { column: 0 };
    for (ply, compact) in game.moves.iter().enumerate() {
        let number = board.fullmoves().get();
        if board.turn() == Color::White {
            wrapper.token(out, &format!("{number}."));
        } else if ply == 0 {
            wrapper.token(out, &format!("{number}..."));
        }
        let mv = legal_move(*compact, &board, game_id, ply)?;
        let san = SanPlus::from_move_and_play_unchecked(&mut board, mv);
        wrapper.token(out, &san.to_string());
    }
    wrapper.token(out, game.metadata.result.to_str());
    out.push('\n');
    Ok(())
}

fn legal_move(
    compact: CompactMove,
    board: &Chess,
    game_id: u64,
    ply: usize,
) -> Result<shakmaty::Move> {
    compact.to_shakmaty_move(board).ok_or_else(|| {
        anyhow!(
            "game {game_id}: stored move {} at ply {} is not legal in its position",
            compact.to_uci(),
            ply + 1
        )
    })
}

struct MovetextWrapper {
    column: usize,
}

impl MovetextWrapper {
    fn token(&mut self, out: &mut String, token: &str) {
        if self.column == 0 {
            out.push_str(token);
            self.column = token.len();
        } else if self.column + 1 + token.len() > MOVETEXT_WIDTH {
            out.push('\n');
            out.push_str(token);
            self.column = token.len();
        } else {
            out.push(' ');
            out.push_str(token);
            self.column += 1 + token.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opening_explorer_core::{Collection, GameMetadata, GameResult};

    fn game_with(
        moves: Vec<CompactMove>,
        fen_start: Option<&str>,
        result: GameResult,
    ) -> ParsedGame {
        ParsedGame {
            metadata: GameMetadata {
                id: 42,
                collection: Collection::MastersOtb,
                white: "Carlsen, Magnus".into(),
                black: "Nakamura, Hikaru".into(),
                white_elo: Some(2850),
                black_elo: Some(2800),
                date: Some("2024.01.15".into()),
                event: Some("Tata Steel 2024".into()),
                site: Some("Wijk aan Zee NED".into()),
                round: Some("5".into()),
                result,
                eco: Some("C65".into()),
                time_control: Some("90+30".into()),
                source_id: Some("https://example.org/game/1".into()),
                broadcast_url: Some("https://example.org/broadcast/1".into()),
                source_attribution: None,
            },
            moves,
            fen_start: fen_start.map(str::to_string),
        }
    }

    fn record(tag: &str) -> AttributionRecord {
        AttributionRecord {
            source_tag: Some(tag.into()),
            white_fide_id: Some("1001".into()),
            black_fide_id: None,
            mode: None,
            white_title: None,
            black_title: None,
        }
    }

    fn attribution(records: Vec<AttributionRecord>) -> AttributionLine {
        AttributionLine {
            schema_version: 1,
            game_id: 42,
            source_attributions: records,
        }
    }

    fn render(game: &ParsedGame, attributions: &AttributionLine) -> Result<(String, ExportStats)> {
        let mut stats = ExportStats::default();
        let mut out = String::new();
        render_game(game, attributions, &mut stats, &mut out)?;
        Ok((out, stats))
    }

    fn e4_e5_nf3_nc6() -> Vec<CompactMove> {
        vec![
            CompactMove::new(12, 28, None), // e2e4
            CompactMove::new(52, 36, None), // e7e5
            CompactMove::new(6, 21, None),  // g1f3
            CompactMove::new(57, 42, None), // b8c6
        ]
    }

    #[test]
    fn standard_game_renders_exact_text() {
        let game = game_with(e4_e5_nf3_nc6(), None, GameResult::Draw);
        let (text, stats) = render(&game, &attribution(vec![record("Britbase")])).unwrap();
        let expected = concat!(
            "[Event \"Tata Steel 2024\"]\n",
            "[Site \"Wijk aan Zee NED\"]\n",
            "[Date \"2024.01.15\"]\n",
            "[Round \"5\"]\n",
            "[White \"Carlsen, Magnus\"]\n",
            "[Black \"Nakamura, Hikaru\"]\n",
            "[Result \"1/2-1/2\"]\n",
            "[WhiteElo \"2850\"]\n",
            "[BlackElo \"2800\"]\n",
            "[WhiteFideId \"1001\"]\n",
            "[ECO \"C65\"]\n",
            "[TimeControl \"90+30\"]\n",
            "[PlyCount \"4\"]\n",
            "[Source \"Britbase\"]\n",
            "[GameURL \"https://example.org/game/1\"]\n",
            "[BroadcastURL \"https://example.org/broadcast/1\"]\n",
            "\n",
            "1. e4 e5 2. Nf3 Nc6 1/2-1/2\n",
            "\n",
        );
        assert_eq!(text, expected);
        assert_eq!(stats.missing_source_tag, 0);
        assert_eq!(stats.setup_games, 0);
    }

    #[test]
    fn black_to_move_start_uses_ellipsis_and_setup_tags() {
        // Position after 1. e4, Black to move.
        let fen = "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1";
        let moves = vec![
            CompactMove::new(52, 36, None), // e7e5
            CompactMove::new(6, 21, None),  // g1f3
        ];
        let game = game_with(moves, Some(fen), GameResult::Star);
        let (text, stats) = render(&game, &attribution(vec![record("Britbase")])).unwrap();
        assert!(text.contains(&format!("[SetUp \"1\"]\n[FEN \"{fen}\"]\n")));
        assert!(
            text.ends_with("\n1... e5 2. Nf3 *\n\n"),
            "movetext was: {text}"
        );
        assert_eq!(stats.setup_games, 1);
    }

    #[test]
    fn illegal_stored_move_is_rejected() {
        let game = game_with(
            vec![CompactMove::new(12, 36, None)], // e2e5 is not legal
            None,
            GameResult::Star,
        );
        let error = render(&game, &attribution(vec![record("Britbase")])).unwrap_err();
        assert!(error.to_string().contains("not legal"), "{error}");
    }

    #[test]
    fn illegal_fen_start_is_rejected() {
        let game = game_with(
            vec![],
            Some("8/8/8/8/8/8/8/8 w - - 0 1"), // no kings
            GameResult::Star,
        );
        assert!(render(&game, &attribution(vec![record("Britbase")])).is_err());
    }

    #[test]
    fn control_characters_in_tags_fail_closed() {
        let mut game = game_with(e4_e5_nf3_nc6(), None, GameResult::Draw);
        game.metadata.white = "Line\nBreak".into();
        let error = render(&game, &attribution(vec![record("Britbase")])).unwrap_err();
        assert!(error.to_string().contains("control character"), "{error}");
    }

    #[test]
    fn quotes_and_backslashes_are_escaped() {
        let mut game = game_with(e4_e5_nf3_nc6(), None, GameResult::Draw);
        game.metadata.white = r#"O"Hara\Smith"#.into();
        let (text, _) = render(&game, &attribution(vec![record("Britbase")])).unwrap();
        assert!(text.contains(r#"[White "O\"Hara\\Smith"]"#), "{text}");
    }

    #[test]
    fn movetext_wraps_before_column_eighty() {
        // Shuttle both knights back and forth: 120 legal plies of valid SAN.
        let shuttle = [
            (6, 21),  // g1f3
            (62, 45), // g8f6
            (21, 6),  // f3g1
            (45, 62), // f6g8
        ];
        let moves: Vec<CompactMove> = (0..120)
            .map(|i| {
                let (from, to) = shuttle[i % 4];
                CompactMove::new(from, to, None)
            })
            .collect();
        let game = game_with(moves, None, GameResult::Draw);
        let (text, _) = render(&game, &attribution(vec![record("Britbase")])).unwrap();
        let movetext: Vec<&str> = text.split("\n\n").nth(1).unwrap().lines().collect();
        assert!(movetext.len() > 1);
        assert!(movetext.iter().all(|line| line.len() <= MOVETEXT_WIDTH));
        assert!(movetext.last().unwrap().ends_with("1/2-1/2"));
    }

    #[test]
    fn conflicting_titles_are_omitted_and_counted() {
        let game = game_with(e4_e5_nf3_nc6(), None, GameResult::Draw);
        let mut first = record("Britbase");
        first.white_title = Some("GM".into());
        let mut second = record("Historical Archive");
        second.white_title = Some("IM".into());
        let (text, stats) = render(&game, &attribution(vec![first, second])).unwrap();
        assert!(!text.contains("WhiteTitle"));
        assert_eq!(stats.omitted_conflicting_tags.get("WhiteTitle"), Some(&1));
        assert!(text.contains("[Source \"Britbase; Historical Archive\"]"));
    }

    #[test]
    fn missing_source_tag_is_counted_not_invented() {
        let game = game_with(e4_e5_nf3_nc6(), None, GameResult::Draw);
        let mut blank = record("");
        blank.source_tag = None;
        let (text, stats) = render(&game, &attribution(vec![blank])).unwrap();
        assert!(!text.contains("[Source "));
        assert_eq!(stats.missing_source_tag, 1);
    }

    fn sidecar_line(game_id: u64) -> String {
        format!(
            "{{\"schema_version\":1,\"game_id\":{game_id},\"source_attributions\":[{{\"source_tag\":\"Britbase\"}}]}}\n"
        )
    }

    #[test]
    fn sidecar_index_rejects_count_mismatch_and_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source-attribution.jsonl");

        fs::write(&path, format!("{}{}", sidecar_line(2), sidecar_line(1))).unwrap();
        let mut index = AttributionIndex::open(&path, 2).unwrap();
        assert_eq!(index.take(1).unwrap().game_id, 1);
        assert_eq!(index.take(2).unwrap().game_id, 2);
        assert!(index
            .take(1)
            .unwrap_err()
            .to_string()
            .contains("more than once"));
        assert!(index
            .take(3)
            .unwrap_err()
            .to_string()
            .contains("no source attribution"));

        assert!(AttributionIndex::open(&path, 3).is_err());

        fs::write(&path, format!("{}{}", sidecar_line(7), sidecar_line(7))).unwrap();
        let error = AttributionIndex::open(&path, 2)
            .err()
            .expect("duplicate ids must be rejected");
        assert!(error.to_string().contains("duplicate"));
    }

    #[test]
    fn sidecar_index_rejects_malformed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source-attribution.jsonl");
        fs::write(
            &path,
            "{\"schema_version\":2,\"game_id\":1,\"source_attributions\":[]}\n",
        )
        .unwrap();
        assert!(AttributionIndex::open(&path, 1).is_err());
        fs::write(&path, "not json\n").unwrap();
        assert!(AttributionIndex::open(&path, 1).is_err());
    }

    fn fixture_game(
        seed: &str,
        white: &str,
        moves: Vec<CompactMove>,
        fen_start: Option<&str>,
        result: GameResult,
    ) -> ParsedGame {
        use opening_explorer_core::compute_stable_game_id;
        let id = compute_stable_game_id(
            Collection::MastersOtb,
            Some(seed),
            white,
            "Opponent",
            Some("2025.01.15"),
            Some("1"),
            &moves,
        );
        let mut game = game_with(moves, fen_start, result);
        game.metadata.id = id;
        game.metadata.white = white.into();
        game.metadata.black = "Opponent".into();
        game.metadata.source_id = Some(seed.into());
        game
    }

    #[test]
    fn export_writes_each_canonical_game_once_and_is_deterministic() {
        use opening_explorer_core::{IngestionCompletionStatus, IngestionStats};
        use opening_explorer_storage::{
            compute_file_sha256, CurationPolicyMetadata, DatasetWriter, DatasetWriterConfig,
            FileChecksumMetadata,
        };

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("masters-otb");
        let config = DatasetWriterConfig {
            collection: Collection::MastersOtb,
            block_capacity: 2,
            max_games_per_shard: 3,
            zstd_level: 3,
            curation_policy: CurationPolicyMetadata {
                min_rating_both: 2200,
                min_rating_one: 2400,
                min_rating_other: 2000,
                min_plies: 4,
                extra_criteria: "export-pgn test".into(),
            },
            deduplication_policy_version: "test".into(),
        };
        let mut writer = DatasetWriter::new(target.clone(), config).unwrap();
        let fen_after_e4 = "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 1";
        let games = vec![
            fixture_game(
                "g1",
                "Alpha, One",
                e4_e5_nf3_nc6(),
                None,
                GameResult::WhiteWins,
            ),
            fixture_game(
                "g2",
                "Beta, Two",
                vec![
                    CompactMove::new(11, 27, None), // d2d4
                    CompactMove::new(51, 35, None), // d7d5
                    CompactMove::new(10, 26, None), // c2c4
                    CompactMove::new(52, 44, None), // e7e6
                ],
                None,
                GameResult::Draw,
            ),
            fixture_game(
                "g3",
                "Gamma, Three",
                vec![
                    CompactMove::new(52, 36, None), // e7e5
                    CompactMove::new(6, 21, None),  // g1f3
                ],
                Some(fen_after_e4),
                GameResult::Star,
            ),
        ];
        let sidecar_path = writer.staging_directory().join("source-attribution.jsonl");
        let mut sidecar = String::new();
        // The storage writer requires sidecar lines in ascending game-ID order (as in
        // games.idx). The exporter does not depend on that order; see
        // `sidecar_index_rejects_count_mismatch_and_duplicates`.
        let mut ids: Vec<u64> = games.iter().map(|g| g.metadata.id).collect();
        ids.sort_unstable();
        for id in &ids {
            sidecar.push_str(&format!(
                "{{\"schema_version\":1,\"game_id\":{id},\"source_attributions\":[{{\"source_tag\":\"Britbase\",\"white_fide_id\":\"1001\"}}]}}\n"
            ));
        }
        for game in games {
            writer.push_game(game).unwrap();
        }
        fs::write(&sidecar_path, &sidecar).unwrap();
        let metadata = FileChecksumMetadata {
            filename: "source-attribution.jsonl".into(),
            file_size_bytes: fs::metadata(&sidecar_path).unwrap().len(),
            sha256: compute_file_sha256(&sidecar_path).unwrap(),
            entry_count: ids.len() as u64,
        };
        let stats = IngestionStats {
            total_encountered: 3,
            accepted_unique: 3,
            confirmed_duplicates: 0,
            superseded_prior: 0,
            rejected_by_policy: 0,
            rejected_variant: 0,
            rejected_illegal: 0,
            rejected_malformed: 0,
            quarantined: 0,
            completion_status: IngestionCompletionStatus::CompleteEof,
        };
        writer
            .finalize_with_source_attribution(stats, Vec::new(), Some(metadata))
            .unwrap();

        let plain_a = dir.path().join("a.pgn");
        let plain_b = dir.path().join("b.pgn");
        let zst = dir.path().join("c.pgn.zst");
        let report = dir.path().join("report.json");
        let export = |output: &Path, report: Option<PathBuf>| {
            run_export_pgn(&ExportOptions {
                dataset: target.clone(),
                output: Some(output.to_path_buf()),
                zstd_level: 3,
                report,
            })
            .unwrap()
        };
        let counted = export(&plain_a, Some(report.clone()));
        export(&plain_b, None);
        export(&zst, None);

        assert_eq!(counted.games_written, 3);
        assert_eq!(counted.setup_games, 1);
        let text_a = fs::read_to_string(&plain_a).unwrap();
        assert_eq!(text_a.matches("[Event ").count(), 3);
        assert_eq!(
            text_a,
            fs::read_to_string(&plain_b).unwrap(),
            "rerun must be byte-identical"
        );
        let decoded = zstd::decode_all(fs::File::open(&zst).unwrap()).unwrap();
        assert_eq!(
            decoded,
            text_a.as_bytes(),
            ".zst must decode to the plain PGN"
        );
        assert!(text_a.contains("[SetUp \"1\"]"));
        assert!(text_a.contains("1... e5 2. Nf3 *"));
        let report_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&report).unwrap()).unwrap();
        assert_eq!(report_json["games_written"], 3);
        assert_eq!(report_json["expected_games"], 3);
        assert!(fs::read_dir(dir.path()).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("partial")));
    }

    #[test]
    fn existing_output_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.pgn");
        fs::write(&path, "keep").unwrap();
        assert!(ensure_absent(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "keep");
    }
}
