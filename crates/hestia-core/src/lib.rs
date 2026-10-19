//! Vocabulary shared by every part of hestia. Pure data, no I/O.

mod command;
mod device;
mod point;
mod value;

pub use command::{Command, Origin};
pub use device::{DeviceId, DeviceIdError, DeviceInfo};
pub use point::{Access, PointKind, PointSpec, Semantic, Unit};
pub use value::{Rgb, Value, ValueError};
