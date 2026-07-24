use std::fs;
use std::process::Command;

#[test]
fn abi_v1_machine_readable_artifacts_match_golden_fixtures() {
    assert_eq!(
        sigilc::ROUTING_HASH_VERSION,
        sigil_rt::ROUTING_HASH_VERSION,
        "compiler and runtime routing contracts diverged"
    );
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.parent().expect("workspace");
    let fixture = manifest.join("tests/fixtures/abi_v1");
    let output = workspace
        .join("target")
        .join(format!("artifact-compatibility-{}", std::process::id()));
    let status = Command::new(env!("CARGO_BIN_EXE_sigilc"))
        .arg(fixture.join("input.sigil"))
        .arg(&output)
        .args(["--level", "1"])
        .status()
        .expect("execute compiler");
    assert!(status.success(), "fixture generation failed");

    assert_eq!(
        fs::read_to_string(output.join("SIGIL_EFFECTS.json")).expect("generated effects"),
        fs::read_to_string(fixture.join("effects.json")).expect("golden effects")
    );
    let residual = fs::read_to_string(output.join("RESIDUAL_RISK.json"))
        .expect("generated residual-risk schema");
    assert!(residual.contains("\"schema_version\": 1"));
    assert!(residual.contains("\"generated_abi\": 1"));
    assert!(residual.contains("\"owner\":\"application_owner\""));
    assert!(residual.contains("\"owner\":\"deployment_owner\""));
    assert!(residual.contains("\"owner\":\"operations_owner\""));
    assert!(residual.contains("\"owner\":\"platform_owner\""));

    let build = fs::read_to_string(output.join("SIGIL_BUILD.json")).expect("build manifest");
    for field in [
        "\"compiler\"",
        "\"language\"",
        "\"runtime\"",
        "\"generated_abi\": 1",
        "\"residual_risk_schema\": 1",
        "\"routing_hash\": 1",
        "\"msrv\": \"1.85.0\"",
        "\"verification_toolchain\": \"1.97.0\"",
        "\"source_sha256\"",
        "\"workspace_lock_sha256\"",
    ] {
        assert!(build.contains(field), "build manifest missing {field}");
    }
}
