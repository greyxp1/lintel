#![no_std]
#![no_main]

mod entry;

use core::ffi::{c_char, c_int};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn main(argc: isize, argv: *const *mut c_char) -> c_int {
  match unsafe { rustbg::cli::parse(argc, argv) } {
    Ok(config) => rustbg::run(config),
    Err(status) => status,
  }
}
