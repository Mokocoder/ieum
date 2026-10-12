#[cfg(not(feature = "python"))]
mod absent;
#[cfg(feature = "python")]
mod embedded;

#[cfg(not(feature = "python"))]
pub(crate) use absent::Foreign;
#[cfg(feature = "python")]
pub(crate) use embedded::Foreign;
