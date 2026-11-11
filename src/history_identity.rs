//! SQLite-free preservation types and bounded metadata handles.
use serde::{Deserialize, Serialize};
pub type ImportId = [u8; 32];
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Column {
    Id,
    Name,
    Group,
    Poster,
    Posted,
    Size,
    Complete,
    Parts,
    FileTotal,
    Display,
    Obfuscated,
    NameSource,
    Category,
    Reason,
    FrozenAt,
    Payload,
    Articles,
    Alias,
    Target,
    FactValue,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum CellType {
    Text,
    Blob,
}
impl Column {
    pub fn name(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Name => "name",
            Self::Group => "group_name",
            Self::Poster => "poster",
            Self::Posted => "posted_date",
            Self::Size => "size",
            Self::Complete => "complete",
            Self::Parts => "parts",
            Self::FileTotal => "file_total",
            Self::Display => "display_name",
            Self::Obfuscated => "is_obfuscated",
            Self::NameSource => "name_source",
            Self::Category => "category",
            Self::Reason => "reason",
            Self::FrozenAt => "frozen_at",
            Self::Payload => "payload",
            Self::Articles => "articles",
            Self::Alias => "key",
            Self::Target => "release_id",
            Self::FactValue => "value",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Value {
    Null,
    Integer(i64),
    RealBits(u64),
    Inline { kind: CellType, bytes: Vec<u8> },
    Bytes { object_id: i64, offset: u64, length: u64, kind: CellType, digest: [u8; 32] },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Metadata {
    pub version: u32,
    pub source_id: i64,
    pub source_key: i64,
    pub fields: Vec<(Column, Value)>,
}
impl Metadata {
    pub fn field(&self, column: Column) -> Option<&Value> {
        self.fields.iter().find(|(c, _)| *c == column).map(|(_, v)| v)
    }
    pub fn effective_name(&self) -> Option<&Value> {
        self.field(Column::Display).filter(|v| !matches!(v, Value::Null)).or_else(|| self.field(Column::Name))
    }
}
