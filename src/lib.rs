#[macro_use]
extern crate napi_derive;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

pub mod algorithms;
pub mod cache;
pub mod manager;

/// Version of the compiled native addon. index.js compares it with package.json to
/// detect a JS wrapper running against a binary from another release.
#[napi]
pub fn native_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
