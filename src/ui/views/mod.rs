//! One module per tab. Each exposes a `render` that draws into the content area; all
//! state lives on [`crate::ui::app::App`], so views stay pure drawing code.

pub mod cache;
pub mod dashboard;
pub mod help;
pub mod hub;
pub mod jobs;
pub mod logs;
pub mod models;
pub mod requests;
pub mod serve;
pub mod templates;
