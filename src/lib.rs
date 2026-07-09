#[macro_use]
extern crate napi_derive;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

pub mod algorithms;
pub mod cache;
pub mod manager;
