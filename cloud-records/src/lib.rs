mod document;
mod template;

pub use document::{DocumentError, JsonDocument};
pub use template::{
    DEFAULT_LOGICAL_BYTES, MAX_BATCH_OPS, MAX_KEY_BYTES, MAX_LIST_LIMIT, MAX_LIST_OFFSET,
    MAX_REQUEST_BYTES, TEMPLATE_ID, TemplateError, records_router, records_service,
    records_service_with_limit, validate_batch, validate_key,
};

mod configured;
pub use configured::ConfiguredService;

mod freeze;
pub use freeze::with_write_freeze;
