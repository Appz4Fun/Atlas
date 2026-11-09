//! One way to run a query on either backend, so a table's code is written once and runs on SQLite today and on
//! Postgres after the move (`[storage] backend`), tested on both.
//!
//! A query is written in the SQL both understand:
//! - parameters are `$1`, `$2`, ... (rewritten to SQLite's `?1`, `?2`, ...);
//! - a parameter whose type the query doesn't give it (in a `values` list, or `$1 is null`) is cast:
//!   `cast($1 as bigint)`, `cast($1 as text)`, since Postgres must know every parameter's type;
//! - `insert ... on conflict ... do nothing / do update`, never `insert or ignore / or replace`;
//! - `case` in place of SQLite's two-argument `min`/`max` and its `sum(x = y)`; a `sum` is cast to bigint
//!   (Postgres sums bigints to numeric);
//! - no temp tables, pragmas or `sqlite_master` (`Sql::has_table` asks the backend).
//!
//! Numbers are read back as `i64` whatever their width, booleans as 0 or 1, as SQLite has them.

use std::cell::RefCell;
use std::fmt;

/// Which backend a `Sql` runs on, for the little that can't be written once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
    Postgres,
}

#[derive(Debug)]
pub enum Error {
    Sqlite(rusqlite::Error),
    Postgres(postgres::Error),
    /// a value that isn't what the caller reads it as, or a query that returned no row where one was needed
    Value(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Sqlite(e) => e.fmt(f),
            Error::Postgres(e) => e.fmt(f),
            Error::Value(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Sqlite(e) => Some(e),
            Error::Postgres(e) => Some(e),
            Error::Value(_) => None,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Sqlite(e)
    }
}

impl From<postgres::Error> for Error {
    fn from(e: postgres::Error) -> Self {
        Error::Postgres(e)
    }
}

/// So the callers still written against rusqlite take this module's errors with `?`. A SQLite error comes back as
/// it was; any other is wrapped (its message kept).
impl From<Error> for rusqlite::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Sqlite(e) => e,
            e => rusqlite::Error::UserFunctionError(Box::new(e)),
        }
    }
}

impl Error {
    /// The Postgres error under this one, when it is one.
    pub fn postgres(&self) -> Option<&postgres::Error> {
        match self {
            Error::Postgres(e) => Some(e),
            _ => None,
        }
    }
}

/// A parameter or a column's value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Int(v)
    }
}
impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Value::Int(v.into())
    }
}
impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Int(v.into())
    }
}
impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Real(v)
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Text(v.to_string())
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Text(v)
    }
}
impl From<&String> for Value {
    fn from(v: &String) -> Self {
        Value::Text(v.clone())
    }
}
impl From<&[u8]> for Value {
    fn from(v: &[u8]) -> Self {
        Value::Blob(v.to_vec())
    }
}
impl From<Vec<u8>> for Value {
    fn from(v: Vec<u8>) -> Self {
        Value::Blob(v)
    }
}
impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map_or(Value::Null, Into::into)
    }
}

/// The parameters of a query: `vals![group, day, now]`.
#[macro_export]
macro_rules! vals {
    () => { &[] as &[$crate::pg::sql::Value] };
    ($($v:expr),+ $(,)?) => { &[$($crate::pg::sql::Value::from($v)),+] as &[$crate::pg::sql::Value] };
}
pub use vals;

/// What a column's `Value` is read as.
pub trait FromValue: Sized {
    fn from_value(v: Value) -> Result<Self>;
}

fn wrong(want: &str, v: &Value) -> Error {
    Error::Value(format!("read {v:?} as {want}"))
}

impl FromValue for Value {
    fn from_value(v: Value) -> Result<Self> {
        Ok(v)
    }
}
impl FromValue for i64 {
    fn from_value(v: Value) -> Result<Self> {
        match v {
            Value::Int(i) => Ok(i),
            v => Err(wrong("an integer", &v)),
        }
    }
}
impl FromValue for bool {
    fn from_value(v: Value) -> Result<Self> {
        i64::from_value(v).map(|i| i != 0)
    }
}
impl FromValue for f64 {
    fn from_value(v: Value) -> Result<Self> {
        match v {
            Value::Real(f) => Ok(f),
            Value::Int(i) => Ok(i as f64),
            v => Err(wrong("a number", &v)),
        }
    }
}
impl FromValue for String {
    fn from_value(v: Value) -> Result<Self> {
        match v {
            Value::Text(s) => Ok(s),
            v => Err(wrong("text", &v)),
        }
    }
}
impl FromValue for Vec<u8> {
    fn from_value(v: Value) -> Result<Self> {
        match v {
            Value::Blob(b) => Ok(b),
            v => Err(wrong("a blob", &v)),
        }
    }
}
impl<T: FromValue> FromValue for Option<T> {
    fn from_value(v: Value) -> Result<Self> {
        match v {
            Value::Null => Ok(None),
            v => T::from_value(v).map(Some),
        }
    }
}

/// A row of a query's answer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row(pub Vec<Value>);

impl Row {
    /// Column `i` (from 0) as a `T`.
    pub fn get<T: FromValue>(&self, i: usize) -> Result<T> {
        let v = self.0.get(i).ok_or_else(|| Error::Value(format!("no column {i}")))?;
        T::from_value(v.clone())
    }
}

/// A connection, or a transaction on one, of either backend.
pub trait Sql {
    fn dialect(&self) -> Dialect;
    /// Runs a statement; the rows it changed.
    fn execute(&self, sql: &str, params: &[Value]) -> Result<usize>;
    /// Every row a query returns.
    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>>;
    /// `execute` for a statement whose text is built per call (a list of n values, a time written in): not kept
    /// in SQLite's statement cache, where it would push out the statements run over and over.
    fn execute_uncached(&self, sql: &str, params: &[Value]) -> Result<usize> {
        self.execute(sql, params)
    }
    /// `query` for a query whose text is built per call (see `execute_uncached`).
    fn query_uncached(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        self.query(sql, params)
    }
    /// Statements without parameters, `;`-separated.
    /// The first row of `query`, reading no further (as rusqlite's `query_row` did).
    fn query_first(&self, sql: &str, params: &[Value]) -> Result<Option<Row>> {
        Ok(self.query(sql, params)?.into_iter().next())
    }

    fn batch(&self, sql: &str) -> Result<()>;
    /// Whether the table exists (in the schema queries name tables in).
    fn has_table(&self, table: &str) -> Result<bool>;
    /// Runs `f` in a transaction, committed when it returns Ok, else rolled back; nested in one already, a
    /// savepoint. See `tx`. A statement that fails inside `f` must fail `f`, or be caught only around a nested
    /// `tx`: on Postgres the failure aborts the transaction, and its commit is then a rollback that reports
    /// success.
    fn transaction(&self, f: &mut dyn FnMut(&dyn Sql) -> Result<()>) -> Result<()>;
}

impl dyn Sql + '_ {
    /// The first row, when there is one.
    pub fn row(&self, sql: &str, params: &[Value]) -> Result<Option<Row>> {
        self.query_first(sql, params)
    }

    /// The first column of the first row; an error when there's no row.
    pub fn one<T: FromValue>(&self, sql: &str, params: &[Value]) -> Result<T> {
        match self.row(sql, params)? {
            Some(r) => r.get(0),
            None => Err(Error::Value(format!("no row from: {sql}"))),
        }
    }

    /// The first column of the first row, None when there's no row.
    pub fn opt<T: FromValue>(&self, sql: &str, params: &[Value]) -> Result<Option<T>> {
        self.row(sql, params)?.map(|r| r.get(0)).transpose()
    }

    /// The first column of every row.
    pub fn column<T: FromValue>(&self, sql: &str, params: &[Value]) -> Result<Vec<T>> {
        self.query(sql, params)?.iter().map(|r| r.get(0)).collect()
    }

    /// Whether the query returns any row. Asked as `select exists (...)`, soo the database stops at the first row:
    /// `row` reads every row of its query, and a chunk check without a limit read every pending day of every group
    /// under the main connection's lock, which stalled every pass on prod.
    pub fn exists(&self, sql: &str, params: &[Value]) -> Result<bool> {
        self.one::<bool>(&format!("select exists ({sql})"), params)
    }

    /// Waits for, and holds to the end of the transaction this runs in, the lock named `name`: what's done under
    /// it runs one at a time across connections, as everything does on SQLite's one writer. Nothing on SQLite.
    /// A query that picks a row then changes it (a claim) can otherwise pick the row another connection is
    /// changing at the same moment, on Postgres, where each statement sees the rows as they were when it began.
    pub fn lock(&self, name: &str) -> Result<()> {
        if self.dialect() == Dialect::Postgres {
            self.execute("select pg_advisory_xact_lock(hashtext($1 || '.' || current_schema()))", vals![name])?;
        }
        Ok(())
    }

    /// `f`'s answer, run in a transaction (see `Sql::transaction`).
    pub fn tx<T>(&self, f: impl FnOnce(&dyn Sql) -> Result<T>) -> Result<T> {
        let mut f = Some(f);
        let mut out = None;
        self.transaction(&mut |s| {
            let f = f.take().ok_or_else(|| Error::Value("a transaction ran its work twice".into()))?;
            out = Some(f(s)?);
            Ok(())
        })?;
        out.ok_or_else(|| Error::Value("a transaction ended without its work".into()))
    }
}

// ---------------------------------------------------------------- SQLite

/// `$1`-style parameters as SQLite's `?1`, outside quotes and comments.
pub fn sqlite_sql(sql: &str) -> std::borrow::Cow<'_, str> {
    if !sql.contains('$') {
        return sql.into();
    }
    let b = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b'\'' | b'"' | b'`' | b'[' => {
                // to the closing quote; a doubled one is part of the text
                let c = if c == b'[' { b']' } else { c };
                let start = i;
                i += 1;
                while i < b.len() {
                    if b[i] == c {
                        if c != b']' && b.get(i + 1) == Some(&c) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i = (i + 1).min(b.len());
                out.push_str(&sql[start..i]);
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                let end = sql[i..].find('\n').map_or(b.len(), |n| i + n);
                out.push_str(&sql[i..end]);
                i = end;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let end = sql[i + 2..].find("*/").map_or(b.len(), |n| i + 2 + n + 2);
                out.push_str(&sql[i..end]);
                i = end;
            }
            b'$' if b.get(i + 1).is_some_and(u8::is_ascii_digit)
                && !(i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) =>
            {
                out.push('?');
                i += 1;
            }
            _ => {
                let ch = sql[i..].chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out.into()
}

impl rusqlite::ToSql for Value {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        use rusqlite::types::{ToSqlOutput, ValueRef};
        Ok(ToSqlOutput::Borrowed(match self {
            Value::Null => ValueRef::Null,
            Value::Int(i) => ValueRef::Integer(*i),
            Value::Real(f) => ValueRef::Real(*f),
            Value::Text(s) => ValueRef::Text(s.as_bytes()),
            Value::Blob(b) => ValueRef::Blob(b),
        }))
    }
}

fn sqlite_value(v: rusqlite::types::ValueRef<'_>) -> Result<Value> {
    use rusqlite::types::ValueRef;
    Ok(match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::Int(i),
        ValueRef::Real(f) => Value::Real(f),
        ValueRef::Text(t) => {
            Value::Text(String::from_utf8(t.to_vec()).map_err(|e| Error::Value(format!("text that isn't UTF-8: {e}")))?)
        }
        ValueRef::Blob(b) => Value::Blob(b.to_vec()),
    })
}

fn sqlite_rows(stmt: &mut rusqlite::Statement<'_>, params: &[Value]) -> Result<Vec<Row>> {
    sqlite_rows_upto(stmt, params, usize::MAX)
}

/// At most `most` rows: the statement isn't stepped past them.
fn sqlite_rows_upto(stmt: &mut rusqlite::Statement<'_>, params: &[Value], most: usize) -> Result<Vec<Row>> {
    let n = stmt.column_count();

    let mut rows = stmt.query(rusqlite::params_from_iter(params))?;
    let mut out = Vec::new();
    while out.len() < most
        && let Some(r) = rows.next()?
    {
        let mut row = Vec::with_capacity(n);
        for i in 0..n {
            row.push(sqlite_value(r.get_ref(i)?)?);
        }
        out.push(Row(row));
    }
    Ok(out)
}

impl Sql for rusqlite::Connection {
    fn dialect(&self) -> Dialect {
        Dialect::Sqlite
    }

    fn execute(&self, sql: &str, params: &[Value]) -> Result<usize> {
        Ok(self.prepare_cached(&sqlite_sql(sql))?.execute(rusqlite::params_from_iter(params))?)
    }

    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        sqlite_rows(&mut *self.prepare_cached(&sqlite_sql(sql))?, params)
    }

    fn query_first(&self, sql: &str, params: &[Value]) -> Result<Option<Row>> {
        Ok(sqlite_rows_upto(&mut *self.prepare_cached(&sqlite_sql(sql))?, params, 1)?.into_iter().next())
    }

    fn execute_uncached(&self, sql: &str, params: &[Value]) -> Result<usize> {
        Ok(self.prepare(&sqlite_sql(sql))?.execute(rusqlite::params_from_iter(params))?)
    }

    fn query_uncached(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        sqlite_rows(&mut self.prepare(&sqlite_sql(sql))?, params)
    }

    fn batch(&self, sql: &str) -> Result<()> {
        Ok(self.execute_batch(sql)?)
    }

    fn has_table(&self, table: &str) -> Result<bool> {
        Ok(self.prepare_cached("select 1 from sqlite_master where type = 'table' and name = ?1")?.exists([table])?)
    }

    fn transaction(&self, f: &mut dyn FnMut(&dyn Sql) -> Result<()>) -> Result<()> {
        if self.is_autocommit() {
            // immediate: the write lock first, as a lone update took it. A deferred one that reads before it writes
            // fails at once (no busy wait) when another connection wrote in between, in WAL
            let tx = rusqlite::Transaction::new_unchecked(self, rusqlite::TransactionBehavior::Immediate)?;
            f(&*tx)?;
            return Ok(tx.commit()?);
        }
        // in a transaction already: a savepoint, released or rolled back to
        self.execute_batch("savepoint atlas_sql")?;
        match f(self) {
            Ok(()) => Ok(self.execute_batch("release atlas_sql")?),
            Err(e) => {
                let _ = self.execute_batch("rollback to atlas_sql; release atlas_sql");
                Err(e)
            }
        }
    }
}

impl Sql for rusqlite::Transaction<'_> {
    fn dialect(&self) -> Dialect {
        Dialect::Sqlite
    }
    fn execute(&self, sql: &str, params: &[Value]) -> Result<usize> {
        Sql::execute(&**self, sql, params)
    }
    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        Sql::query(&**self, sql, params)
    }
    fn query_first(&self, sql: &str, params: &[Value]) -> Result<Option<Row>> {
        Sql::query_first(&**self, sql, params)
    }
    fn execute_uncached(&self, sql: &str, params: &[Value]) -> Result<usize> {
        Sql::execute_uncached(&**self, sql, params)
    }
    fn query_uncached(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        Sql::query_uncached(&**self, sql, params)
    }
    fn batch(&self, sql: &str) -> Result<()> {
        Sql::batch(&**self, sql)
    }
    fn has_table(&self, table: &str) -> Result<bool> {
        Sql::has_table(&**self, table)
    }
    fn transaction(&self, f: &mut dyn FnMut(&dyn Sql) -> Result<()>) -> Result<()> {
        Sql::transaction(&**self, f)
    }
}

// ---------------------------------------------------------------- Postgres

impl postgres::types::ToSql for Value {
    fn to_sql(
        &self,
        ty: &postgres::types::Type,
        out: &mut bytes::BytesMut,
    ) -> std::result::Result<postgres::types::IsNull, Box<dyn std::error::Error + Sync + Send>> {
        use postgres::types::Type;
        match self {
            Value::Null => Ok(postgres::types::IsNull::Yes),
            Value::Int(i) => match *ty {
                Type::INT8 => i.to_sql(ty, out),
                Type::INT4 => i32::try_from(*i)?.to_sql(ty, out),
                Type::INT2 => i16::try_from(*i)?.to_sql(ty, out),
                Type::BOOL => (*i != 0).to_sql(ty, out),
                Type::FLOAT8 => (*i as f64).to_sql(ty, out),
                Type::FLOAT4 => (*i as f32).to_sql(ty, out),
                _ => Err(format!("an integer for a parameter of type {ty}").into()),
            },
            Value::Real(f) => match *ty {
                Type::FLOAT8 => f.to_sql(ty, out),
                Type::FLOAT4 => (*f as f32).to_sql(ty, out),
                _ => Err(format!("a number for a parameter of type {ty}").into()),
            },
            // their bytes are taken as is by any type: an 8-byte text for a bigint would read as a number
            Value::Text(s) => match *ty {
                Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::UNKNOWN => s.as_str().to_sql(ty, out),
                _ => Err(format!("text for a parameter of type {ty}").into()),
            },
            Value::Blob(b) => match *ty {
                Type::BYTEA => b.as_slice().to_sql(ty, out),
                _ => Err(format!("a blob for a parameter of type {ty}").into()),
            },
        }
    }

    fn accepts(_: &postgres::types::Type) -> bool {
        true
    }

    postgres::types::to_sql_checked!();
}

fn pg_row(r: &postgres::Row) -> Result<Row> {
    use postgres::types::Type;
    let mut row = Vec::with_capacity(r.len());
    for (i, col) in r.columns().iter().enumerate() {
        let ty = col.type_();
        let v = match *ty {
            Type::INT8 => r.try_get::<_, Option<i64>>(i)?.into(),
            Type::INT4 => r.try_get::<_, Option<i32>>(i)?.map(i64::from).into(),
            Type::INT2 => r.try_get::<_, Option<i16>>(i)?.map(i64::from).into(),
            Type::BOOL => r.try_get::<_, Option<bool>>(i)?.into(),
            Type::FLOAT8 => r.try_get::<_, Option<f64>>(i)?.into(),
            Type::FLOAT4 => r.try_get::<_, Option<f32>>(i)?.map(f64::from).into(),
            Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::UNKNOWN => {
                r.try_get::<_, Option<String>>(i)?.into()
            }
            Type::BYTEA => r.try_get::<_, Option<Vec<u8>>>(i)?.into(),
            _ => return Err(Error::Value(format!("column {} is of type {ty}, which isn't read", col.name()))),
        };
        row.push(v);
    }
    Ok(Row(row))
}

type PgParams<'a> = [&'a (dyn postgres::types::ToSql + Sync)];

/// What `Pg` runs on: a client, a pooled connection, a transaction, or a borrow of one.
pub trait PgClient {
    fn pg_execute(&mut self, sql: &str, params: &PgParams<'_>) -> Result<u64, postgres::Error>;
    fn pg_query(&mut self, sql: &str, params: &PgParams<'_>) -> Result<Vec<postgres::Row>, postgres::Error>;
    fn pg_batch(&mut self, sql: &str) -> Result<(), postgres::Error>;
    fn pg_transaction(&mut self) -> Result<postgres::Transaction<'_>, postgres::Error>;
}

macro_rules! pg_client {
    ($t:ty) => {
        impl PgClient for $t {
            fn pg_execute(&mut self, sql: &str, params: &PgParams<'_>) -> Result<u64, postgres::Error> {
                postgres::GenericClient::execute(self, sql, params)
            }
            fn pg_query(&mut self, sql: &str, params: &PgParams<'_>) -> Result<Vec<postgres::Row>, postgres::Error> {
                postgres::GenericClient::query(self, sql, params)
            }
            fn pg_batch(&mut self, sql: &str) -> Result<(), postgres::Error> {
                postgres::GenericClient::batch_execute(self, sql)
            }
            fn pg_transaction(&mut self) -> Result<postgres::Transaction<'_>, postgres::Error> {
                postgres::GenericClient::transaction(self)
            }
        }
    };
}
pg_client!(postgres::Client);
pg_client!(postgres::Transaction<'_>);

impl PgClient for super::Conn<'_> {
    fn pg_execute(&mut self, sql: &str, params: &PgParams<'_>) -> Result<u64, postgres::Error> {
        (**self).pg_execute(sql, params)
    }
    fn pg_query(&mut self, sql: &str, params: &PgParams<'_>) -> Result<Vec<postgres::Row>, postgres::Error> {
        (**self).pg_query(sql, params)
    }
    fn pg_batch(&mut self, sql: &str) -> Result<(), postgres::Error> {
        (**self).pg_batch(sql)
    }
    fn pg_transaction(&mut self) -> Result<postgres::Transaction<'_>, postgres::Error> {
        (**self).pg_transaction()
    }
}

impl<T: PgClient + ?Sized> PgClient for &mut T {
    fn pg_execute(&mut self, sql: &str, params: &PgParams<'_>) -> Result<u64, postgres::Error> {
        (**self).pg_execute(sql, params)
    }
    fn pg_query(&mut self, sql: &str, params: &PgParams<'_>) -> Result<Vec<postgres::Row>, postgres::Error> {
        (**self).pg_query(sql, params)
    }
    fn pg_batch(&mut self, sql: &str) -> Result<(), postgres::Error> {
        (**self).pg_batch(sql)
    }
    fn pg_transaction(&mut self) -> Result<postgres::Transaction<'_>, postgres::Error> {
        (**self).pg_transaction()
    }
}

/// A Postgres client or transaction as a `Sql`.
pub struct Pg<C>(RefCell<C>);

impl<C: PgClient> Pg<C> {
    pub fn new(c: C) -> Self {
        Pg(RefCell::new(c))
    }

    pub fn into_inner(self) -> C {
        self.0.into_inner()
    }

    fn client(&self) -> Result<std::cell::RefMut<'_, C>> {
        // only a closure handed the outer one inside its own transaction gets here
        self.0.try_borrow_mut().map_err(|_| Error::Value("a connection used inside its own transaction".into()))
    }
}

fn pg_params(params: &[Value]) -> Vec<&(dyn postgres::types::ToSql + Sync)> {
    params.iter().map(|p| p as &(dyn postgres::types::ToSql + Sync)).collect()
}

impl<C: PgClient> Sql for Pg<C> {
    fn dialect(&self) -> Dialect {
        Dialect::Postgres
    }

    fn execute(&self, sql: &str, params: &[Value]) -> Result<usize> {
        Ok(self.client()?.pg_execute(sql, &pg_params(params))? as usize)
    }

    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        self.client()?.pg_query(sql, &pg_params(params))?.iter().map(pg_row).collect()
    }

    fn batch(&self, sql: &str) -> Result<()> {
        Ok(self.client()?.pg_batch(sql)?)
    }

    fn has_table(&self, table: &str) -> Result<bool> {
        let r = self
            .client()?
            .pg_query("select 1 from pg_tables where schemaname = current_schema() and tablename = $1", &[&table])?;
        Ok(!r.is_empty())
    }

    fn transaction(&self, f: &mut dyn FnMut(&dyn Sql) -> Result<()>) -> Result<()> {
        let mut c = self.client()?;
        // a savepoint when this is a transaction already
        let tx = Pg::new(c.pg_transaction()?);
        f(&tx)?;
        Ok(tx.into_inner().commit()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_are_rewritten_outside_quotes_and_comments() {
        assert_eq!(sqlite_sql("select $1, $12 from t"), "select ?1, ?12 from t");
        assert_eq!(sqlite_sql("select '$1', \"$2\" -- $3\n, $4"), "select '$1', \"$2\" -- $3\n, ?4");
        assert_eq!(sqlite_sql("select 'it''s $1', $2"), "select 'it''s $1', ?2");
        assert_eq!(sqlite_sql("select 'é', $1"), "select 'é', ?1");
        assert_eq!(sqlite_sql("select $x"), "select $x");
        assert_eq!(sqlite_sql("select /* $1 */ a$1, $2"), "select /* $1 */ a$1, ?2");
        assert_eq!(sqlite_sql("select [$1], `$2`, $3"), "select [$1], `$2`, ?3");
    }

    fn check(db: &dyn Sql) {
        db.batch("drop table if exists t; create table t (k text primary key, n bigint, f double precision)").unwrap();
        assert!(db.has_table("t").unwrap());
        assert!(!db.has_table("nope").unwrap());
        assert_eq!(db.execute("insert into t (k, n, f) values ($1, $2, $3)", vals!["a", 1i64, 0.5]).unwrap(), 1);
        db.execute("insert into t (k, n) values ($2, $1)", vals![Some(2i64), "b"]).unwrap();
        db.execute("insert into t (k, n) values ($1, $2)", vals!["c", None::<i64>]).unwrap();
        assert_eq!(db.one::<i64>("select count(*) from t", vals![]).unwrap(), 3);
        assert_eq!(db.opt::<Option<i64>>("select n from t where k = $1", vals!["c"]).unwrap(), Some(None));
        assert_eq!(db.opt::<i64>("select n from t where k = $1", vals!["z"]).unwrap(), None);
        assert_eq!(db.column::<String>("select k from t order by k", vals![]).unwrap(), ["a", "b", "c"]);
        assert!(db.exists("select 1 from t where n = $1", vals![2i64]).unwrap());
        assert!(db.one::<bool>("select exists(select 1 from t where k = $1)", vals!["a"]).unwrap());
        assert_eq!(db.one::<f64>("select f from t where k = 'a'", vals![]).unwrap(), 0.5);
        assert_eq!(
            db.one::<i64>("select cast(sum(case when n > 0 then n else 0 end) as bigint) from t", vals![]).unwrap(),
            3
        );
        // a typeless parameter, cast
        assert_eq!(
            db.one::<i64>("select count(*) from t where cast($1 as bigint) is null or n = $1", vals![None::<i64>])
                .unwrap(),
            3
        );
        // committed, rolled back, and nested
        let n = db
            .tx(|t| {
                t.execute("update t set n = 10 where k = $1", vals!["a"])?;
                let _ = t.tx(|s| -> Result<()> {
                    s.execute("update t set n = 20 where k = $1", vals!["b"])?;
                    Err(Error::Value("undo the inner one".into()))
                });
                t.one::<i64>("select n from t where k = 'b'", vals![])
            })
            .unwrap();
        assert_eq!(n, 2, "the savepoint rolled back");
        assert_eq!(db.one::<i64>("select n from t where k = 'a'", vals![]).unwrap(), 10);
        let r: Result<()> = db.tx(|t| {
            t.execute("delete from t", vals![])?;
            Err(Error::Value("no".into()))
        });
        assert!(r.is_err());
        assert_eq!(db.one::<i64>("select count(*) from t", vals![]).unwrap(), 3);
        assert_eq!(db.query_uncached("select k from t where n = $1", vals![10i64]).unwrap().len(), 1);
        if db.dialect() == Dialect::Postgres {
            // text is never taken for a number's bytes
            assert!(db.one::<i64>("select count(*) from t where n = $1", vals!["20000101"]).is_err());
        }
    }

    #[test]
    fn both_backends_answer_alike() {
        crate::pg::testing::each(|_| {}, check);
    }
}
