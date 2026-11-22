//! Event arguments (debug annotations).

/// Value of a debug annotation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ArgValue<'a> {
    Bool(bool),
    Int(i64),
    UInt(u64),
    Double(f64),
    Str(&'a str),
    /// Shown in hex by the UI.
    Pointer(u64),
}

/// A named event argument, stored as a TrackEvent `debug_annotations` entry
/// (visible in trace_processor's `args` table as `debug.<name>`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arg<'a> {
    pub name: &'a str,
    pub value: ArgValue<'a>,
}

impl<'a> Arg<'a> {
    pub fn new(name: &'a str, value: impl Into<ArgValue<'a>>) -> Self {
        Arg { name, value: value.into() }
    }
}

macro_rules! from_int {
    ($($t:ty => $v:ident as $c:ty),*) => {$(
        impl<'a> From<$t> for ArgValue<'a> {
            fn from(x: $t) -> Self { ArgValue::$v(x as $c) }
        }
    )*};
}
from_int!(i8 => Int as i64, i16 => Int as i64, i32 => Int as i64, i64 => Int as i64, isize => Int as i64,
          u8 => UInt as u64, u16 => UInt as u64, u32 => UInt as u64, u64 => UInt as u64, usize => UInt as u64,
          f32 => Double as f64, f64 => Double as f64);

impl<'a> From<bool> for ArgValue<'a> {
    fn from(x: bool) -> Self {
        ArgValue::Bool(x)
    }
}
impl<'a> From<&'a str> for ArgValue<'a> {
    fn from(x: &'a str) -> Self {
        ArgValue::Str(x)
    }
}
impl<'a> From<&'a String> for ArgValue<'a> {
    fn from(x: &'a String) -> Self {
        ArgValue::Str(x.as_str())
    }
}
impl<'a, T> From<*const T> for ArgValue<'a> {
    fn from(x: *const T) -> Self {
        ArgValue::Pointer(x as usize as u64)
    }
}
impl<'a, T> From<*mut T> for ArgValue<'a> {
    fn from(x: *mut T) -> Self {
        ArgValue::Pointer(x as usize as u64)
    }
}

/// A counter sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CounterValue {
    Int(i64),
    Double(f64),
}

impl From<i64> for CounterValue {
    fn from(v: i64) -> Self {
        CounterValue::Int(v)
    }
}
impl From<i32> for CounterValue {
    fn from(v: i32) -> Self {
        CounterValue::Int(v as i64)
    }
}
impl From<u32> for CounterValue {
    fn from(v: u32) -> Self {
        CounterValue::Int(v as i64)
    }
}
impl From<usize> for CounterValue {
    fn from(v: usize) -> Self {
        CounterValue::Int(v as i64)
    }
}
impl From<f64> for CounterValue {
    fn from(v: f64) -> Self {
        CounterValue::Double(v)
    }
}
impl From<f32> for CounterValue {
    fn from(v: f32) -> Self {
        CounterValue::Double(v as f64)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Begin,
    End,
    Instant,
    Counter,
}
