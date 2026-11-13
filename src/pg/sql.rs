//! PostgreSQL query parameters, rows and transaction callbacks.
//!
//! Queries use PostgreSQL `$1` parameters and explicit casts for otherwise
//! untyped values. Integer widths normalize to `i64`, and booleans to 0 or 1.

use std::cell::RefCell;
use std::fmt;

/// The query dialect exposed to existing native SQL callers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Postgres,
}

#[derive(Debug)]
pub enum Error {
    Postgres(postgres::Error),
    /// Native registration/isolation refusal, retaining its complete cause chain.
    NativeGate(anyhow::Error),
    /// a value that isn't what the caller reads it as, or a query that returned no row where one was needed
    Value(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Postgres(e) => e.fmt(f),
            Error::NativeGate(e) => e.fmt(f),
            Error::Value(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Postgres(e) => Some(e),
            Error::NativeGate(e) => Some(e.as_ref()),
            Error::Value(_) => None,
        }
    }
}

impl From<postgres::Error> for Error {
    fn from(e: postgres::Error) -> Self {
        Error::Postgres(e)
    }
}

impl Error {
    /// The Postgres error under this one, when it is one.
    pub fn postgres(&self) -> Option<&postgres::Error> {
        match self {
            Error::Postgres(e) => Some(e),
            Error::NativeGate(e) => e.chain().find_map(|cause| cause.downcast_ref::<postgres::Error>()),
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

/// A PostgreSQL connection or transaction.
pub trait Sql {
    fn dialect(&self) -> Dialect;
    /// Runs a statement; the rows it changed.
    fn execute(&self, sql: &str, params: &[Value]) -> Result<usize>;
    /// Every row a query returns.
    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>>;
    /// `execute` for a statement whose text is built per call (a list of n values, a time written in).
    fn execute_uncached(&self, sql: &str, params: &[Value]) -> Result<usize> {
        self.execute(sql, params)
    }
    /// `query` for a query whose text is built per call (see `execute_uncached`).
    fn query_uncached(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        self.query(sql, params)
    }
    /// The first row returned by `query`.
    fn query_first(&self, sql: &str, params: &[Value]) -> Result<Option<Row>> {
        Ok(self.query(sql, params)?.into_iter().next())
    }

    /// Statements without parameters, `;`-separated.
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

    /// Holds the named transaction lock until commit, before changing rows selected under it.
    pub fn lock(&self, name: &str) -> Result<()> {
        self.execute("select pg_advisory_xact_lock(hashtext($1 || '.' || current_schema()))", vals![name])?;
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
        let mut transaction = c.pg_transaction()?;
        // Fence before the callback can acquire application/group row locks.
        super::history_gate::native(&mut transaction).map_err(Error::NativeGate)?;
        let tx = Pg::new(transaction);
        f(&tx)?;
        Ok(tx.into_inner().commit()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_values_keep_null_binary_boolean_and_type_boundaries() {
        let row = Row(vec![Value::Null, Value::Int(-7), Value::Text("café".into()), Value::Blob(vec![0, 255])]);
        assert_eq!(row.get::<Option<i64>>(0).unwrap(), None);
        assert_eq!(row.get::<i64>(1).unwrap(), -7);
        assert!(row.get::<bool>(1).unwrap());
        assert_eq!(row.get::<f64>(1).unwrap(), -7.0);
        assert_eq!(row.get::<String>(2).unwrap(), "café");
        assert_eq!(row.get::<Vec<u8>>(3).unwrap(), [0, 255]);
        assert!(row.get::<i64>(2).is_err());
        assert!(row.get::<String>(3).is_err());
        assert!(row.get::<Value>(4).is_err());
    }

    #[test]
    fn postgres_parameter_encoding_keeps_fixed_binary_vectors_and_refusals() {
        use postgres::types::{IsNull, ToSql, Type};

        for (value, ty, expected) in [
            (Value::Int(258), Type::INT2, vec![1u8, 2]),
            (Value::Int(-3), Type::INT4, vec![255, 255, 255, 253]),
            (Value::Int(1), Type::BOOL, vec![1]),
            (Value::Real(0.5), Type::FLOAT8, vec![63, 224, 0, 0, 0, 0, 0, 0]),
            (Value::Text("é".into()), Type::TEXT, vec![195, 169]),
            (Value::Blob(vec![0, 255]), Type::BYTEA, vec![0, 255]),
        ] {
            let mut out = bytes::BytesMut::new();
            assert!(matches!(value.to_sql(&ty, &mut out).unwrap(), IsNull::No));
            assert_eq!(&out[..], expected);
        }
        let mut out = bytes::BytesMut::new();
        assert!(matches!(Value::Null.to_sql(&Type::INT8, &mut out).unwrap(), IsNull::Yes));
        assert!(out.is_empty());
        assert!(Value::Int(32768).to_sql(&Type::INT2, &mut out).is_err());
        assert!(Value::Text("20000101".into()).to_sql(&Type::INT8, &mut out).is_err());
        assert!(out.is_empty(), "refused values must not publish partial parameter bytes");
    }

    #[test]
    fn native_gate_errors_keep_typed_causes() {
        let error = Error::NativeGate(
            anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::PermissionDenied)).context("native ownership"),
        );
        let mut cause = std::error::Error::source(&error);
        let mut found = false;
        while let Some(current) = cause {
            if let Some(io) = current.downcast_ref::<std::io::Error>() {
                assert_eq!(io.kind(), std::io::ErrorKind::PermissionDenied);
                found = true;
            }
            cause = current.source();
        }
        assert!(found, "native refusal must retain its typed cause through context");
    }

    #[test]
    fn postgres_driver_causes_remain_discoverable_through_native_gate_context() {
        crate::pg::testing::each(|db| {
            let error = db.query("select * from missing_sql_cause_fixture", vals![]).unwrap_err();
            let error = Error::NativeGate(anyhow::Error::new(error).context("native ownership"));
            assert_eq!(error.postgres().unwrap().code(), Some(&postgres::error::SqlState::UNDEFINED_TABLE));
        });
    }

    #[test]
    fn postgres_fixed_snapshot_refuses_callback_before_mutation() {
        let Some(schema) = crate::pg::testing::Schema::new() else {
            eprintln!("skipping PostgreSQL test: ATLAS_PG_TEST_URL is required");
            return;
        };
        let mut client = schema.pool.get().unwrap();
        client.batch_execute("set default_transaction_isolation='repeatable read'").unwrap();
        let pg = Pg::new(client);
        let db: &dyn Sql = &pg;
        let entered = std::cell::Cell::new(false);
        let result = db.tx(|_| {
            entered.set(true);
            Ok(())
        });
        assert!(matches!(result, Err(Error::NativeGate(_))));
        assert!(!entered.get(), "the native history gate must precede the callback");
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
        assert_eq!(db.one::<Vec<u8>>("select $1::bytea", vals![vec![0u8, 255]]).unwrap(), [0, 255]);
        assert_eq!(
            db.query("select 2::smallint,3::integer,4::bigint,false", vals![]).unwrap(),
            [Row(vec![Value::Int(2), Value::Int(3), Value::Int(4), Value::Int(0)])]
        );
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
        // Text is never taken for a number's bytes.
        assert!(db.one::<i64>("select count(*) from t where n = $1", vals!["20000101"]).is_err());
    }

    #[test]
    fn postgres_queries_and_nested_transactions_preserve_values() {
        crate::pg::testing::each(check);
    }
}
