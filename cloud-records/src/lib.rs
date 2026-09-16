mod document;
mod template;

pub use document::{DocumentError, JsonDocument};
pub use template::{
    MAX_BATCH_OPS, MAX_KEY_BYTES, MAX_LIST_LIMIT, MAX_LIST_OFFSET, MAX_REQUEST_BYTES, TEMPLATE_ID,
    TemplateError, records_router, records_service, validate_batch, validate_key,
};
