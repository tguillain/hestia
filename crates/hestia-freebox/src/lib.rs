//! Freebox through its local Freebox OS API. The application must be authorised once on
//! the box (`hestia freebox register`); the token is then kept in the data folder.

mod api;
mod driver;
mod model;
mod presence;
mod token;

pub use api::{ApiError, ApiVersion, FreeboxApi, password};
pub use driver::{FreeboxConfig, FreeboxDriver};
pub use model::LanHost;
pub use presence::{People, Person, add_person, lan_hosts, load_people, remove_person};
pub use token::{Token, load_token, save_token};
