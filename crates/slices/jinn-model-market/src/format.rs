//! Display formatting for the view: the price and score columns, and the
//! "updated X ago" stamp.
//!
//! Every formatter here lives in the contract crate — the web UI's rows
//! are built through the same functions, so one format cannot drift
//! between the surfaces. This module re-exports them under the names the
//! view and its tests already use.

pub use jinn_model_market_msg::age_stamp;
pub use jinn_model_market_msg::{
    EMPTY_CELL, cache_column, dollars, eci_column, ms_column, percent_column, price_per_million,
    throughput_column, vals_column,
};
