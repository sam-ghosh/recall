mod indexer;
mod schema;
mod state;
mod sync;

pub use indexer::{discover_and_sort_files, index_files, plan_update, IndexProgress, IndexUpdate};
pub use schema::{SearchFilter, SessionIndex};
pub use state::IndexState;
pub use sync::ensure_index_fresh;
