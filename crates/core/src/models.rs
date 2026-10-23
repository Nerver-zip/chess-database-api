use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Collection {
    #[serde(rename = "masters-otb")]
    MastersOtb,
    #[serde(rename = "elite-online")]
    EliteOnline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GameResult {
    WhiteWins,
    Draw,
    BlackWins,
    Star,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameMetadata {
    pub id: u64,
    pub collection: Collection,
    pub white: String,
    pub black: String,
    pub white_elo: Option<u16>,
    pub black_elo: Option<u16>,
    pub date: Option<String>,
    pub event: Option<String>,
    pub site: Option<String>,
    pub round: Option<String>,
    pub result: GameResult,
    pub eco: Option<String>,
    pub time_control: Option<String>,
    pub source_id: Option<String>,
}
