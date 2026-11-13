//! Database-neutral fields of the retained native result v1 wire format.
//! Tags retain their original SQL spelling for receipt compatibility; the
//! native writer never executes them. Only closed StepMutation creates a tag.
use anyhow::{Context, Result, ensure};

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

macro_rules! integer {
    ($($kind:ty),*) => {$(impl From<$kind> for Value {
        fn from(value:$kind)->Self {Self::Integer(i64::from(value))}
    })*};
}
integer!(i8, i16, i32, i64, u8, u16, u32, bool);
impl From<f64> for Value {fn from(value:f64)->Self {Self::Real(value)}}
impl From<String> for Value {fn from(value:String)->Self {Self::Text(value)}}
impl From<Vec<u8>> for Value {fn from(value:Vec<u8>)->Self {Self::Blob(value)}}
impl<T:Into<Value>> From<Option<T>> for Value {
    fn from(value:Option<T>)->Self {value.map_or(Self::Null,Into::into)}
}

fn field(value:&[u8],used:&mut usize,sink:&mut impl FnMut(&[u8]))->Result<()> {
    *used=used.checked_add(value.len()).context("native result size overflow")?;
    ensure!(*used<=64*1024*1024,"native result exceeds byte admission");
    sink(&(value.len() as u64).to_le_bytes());sink(value);Ok(())
}

/// Exactly the old Visit write fragment, sharing the caller's aggregate byte
/// counter. Numeric/framing bytes were not charged by v1 and remain unchanged.
pub(crate) fn encode_write_v1(step:i64,tag:&str,values:&[Value],used:&mut usize,sink:&mut impl FnMut(&[u8]))->Result<()> {
    sink(&step.to_le_bytes());field(tag.as_bytes(),used,sink)?;
    for value in values {
        match value {
            Value::Null=>sink(&[0]),
            Value::Integer(n)=>{sink(&[1]);sink(&n.to_le_bytes());},
            Value::Real(n)=>{sink(&[2]);sink(&n.to_bits().to_le_bytes());},
            Value::Text(text)=>{sink(&[3]);field(text.as_bytes(),used,sink)?;},
            Value::Blob(blob)=>{sink(&[4]);field(blob,used,sink)?;},
        }
    }
    sink(&[255]);Ok(())
}
