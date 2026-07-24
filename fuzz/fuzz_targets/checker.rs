#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(source) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(program) = sigilc::parse(source) else {
        return;
    };
    let Ok(graph) = sigilc::lower(&program) else {
        return;
    };
    let _ = sigilc::run_checks(&program, &graph, sigilc::AssuranceLevel::System);
});
