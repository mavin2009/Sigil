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
    if sigilc::run_checks(&program, &graph, sigilc::AssuranceLevel::Safe).is_ok() {
        let _ = sigilc::emit(&program, &graph);
        let _ = sigilc::emit_demo_main(&program);
        let _ = sigilc::emit_effect_contracts(&program);
        if let Ok(topology) = sigilc::derive_topology(&program) {
            let _ = sigilc::to_dot(&program, &topology);
            let _ = sigilc::to_mermaid(&program, &topology);
        }
    }
});
