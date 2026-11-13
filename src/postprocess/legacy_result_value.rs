//! SQLite compatibility is confined to legacy execution, not native encoding.
use super::result_value::Value;

impl super::mutation::ImageRecord {
    /// Retained API for the legacy image-row reader. Native mutation fields
    /// use v1_values directly and never call this adapter.
    pub(crate) fn sqlite_values(&self)->Vec<rusqlite::types::Value> {
        self.v1_values().into_iter().map(Into::into).collect()
    }
}

impl rusqlite::ToSql for Value {
    fn to_sql(&self)->rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        use rusqlite::types::{ToSqlOutput,ValueRef};
        Ok(ToSqlOutput::Borrowed(match self {
            Self::Null=>ValueRef::Null,
            Self::Integer(value)=>ValueRef::Integer(*value),
            Self::Real(value)=>ValueRef::Real(*value),
            Self::Text(value)=>ValueRef::Text(value.as_bytes()),
            Self::Blob(value)=>ValueRef::Blob(value),
        }))
    }
}

impl From<Value> for rusqlite::types::Value {
    fn from(value:Value)->Self {
        match value {
            Value::Null=>Self::Null,Value::Integer(value)=>Self::Integer(value),
            Value::Real(value)=>Self::Real(value),Value::Text(value)=>Self::Text(value),Value::Blob(value)=>Self::Blob(value),
        }
    }
}
