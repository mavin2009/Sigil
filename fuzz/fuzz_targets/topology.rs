#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(source) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(program) = sigilc::parse(source) {
        let _ = sigilc::derive_topology(&program);
    }
});
