//! Native clipboard data exposed without changing the system pasteboard.
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
#[napi(object)]
pub struct ClipboardImage {
    pub data: Buffer,
    pub media_type: String,
    pub name: String,
}
