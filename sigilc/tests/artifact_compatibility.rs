use std::fs;
use std::process::Command;

#[test]
fn abi_v5_machine_readable_artifacts_match_golden_fixtures() {
    assert_eq!(
        sigilc::ROUTING_HASH_VERSION,
        sigil_rt::ROUTING_HASH_VERSION,
        "compiler and runtime routing contracts diverged"
    );
    assert_eq!(
        sigilc::DISTRIBUTED_PROTOCOL_VERSION,
        sigil_rt::distributed::DISTRIBUTED_PROTOCOL_VERSION,
        "compiler and runtime distributed contracts diverged"
    );
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.parent().expect("workspace");
    let fixture = manifest.join("tests/fixtures/abi_v5");
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
    assert!(residual.contains("\"generated_abi\": 5"));
    assert!(residual.contains("\"owner\":\"application_owner\""));
    assert!(residual.contains("\"owner\":\"deployment_owner\""));
    assert!(residual.contains("\"owner\":\"operations_owner\""));
    assert!(residual.contains("\"owner\":\"platform_owner\""));
    assert!(residual.contains("\"kind\":\"distributed_transport\""));
    assert!(residual.contains("\"kind\":\"distributed_identity\""));
    assert!(residual.contains("\"kind\":\"shard_coordination\""));
    assert!(residual.contains("\"kind\":\"distributed_proof_boundary\""));

    let build = fs::read_to_string(output.join("SIGIL_BUILD.json")).expect("build manifest");
    for field in [
        "\"compiler\"",
        "\"language\"",
        "\"runtime\"",
        "\"generated_abi\": 5",
        "\"residual_risk_schema\": 1",
        "\"routing_hash\": 1",
        "\"distributed_protocol\": 1",
        "\"msrv\": \"1.85.0\"",
        "\"verification_toolchain\": \"1.97.0\"",
        "\"source_sha256\"",
        "\"workspace_lock_sha256\"",
    ] {
        assert!(build.contains(field), "build manifest missing {field}");
    }

    let generated_rust =
        fs::read_to_string(output.join("src/lib.rs")).expect("generated Rust library");
    assert!(generated_rust.contains("pub struct ComponentConfig"));
    assert!(generated_rust.contains("pub struct ComponentHealth"));
    assert!(generated_rust.contains("pub struct Component"));
    assert!(generated_rust.contains("sigil_rt::IngressRouter<PHandle>"));
    assert!(generated_rust.contains("pub fn admit_shed"));
    assert!(generated_rust.contains("pub async fn admit_deadline"));
    assert!(generated_rust.contains("pub const COMPONENT_PLACEMENT"));
    assert!(generated_rust.contains("RemoteBoundaryDescriptor"));
    assert!(generated_rust.contains("pub fn transport_manifest"));
    assert!(generated_rust.contains("pub struct RemoteEndpoints"));
    assert!(generated_rust.contains("sigil_rt::distributed::DurableRemoteEndpoint<M>"));
    assert!(generated_rust.contains("pub struct RemoteCommitters"));
    assert!(generated_rust.contains("pub struct PlacementComponent"));
    assert!(generated_rust.contains("pub async fn deliver_q_m"));
    assert!(generated_rust.contains("StateCommitter<Q>"));
    assert!(generated_rust.contains("impl sigil_rt::distributed::WireCodec for M"));
    assert!(generated_rust.contains("const FINGERPRINT"));
    assert!(generated_rust.contains("pub fn encode_remote("));
    assert!(generated_rust.contains("pub fn authorize_remote("));

    // ABI v1 through v4 remain immutable evidence. Version 5 is additive at
    // the Rust interface but must never be mislabeled as an earlier version.
    let v1_effects =
        fs::read_to_string(manifest.join("tests/fixtures/abi_v1/effects.json")).expect("ABI v1");
    assert!(v1_effects.contains("\"generated_abi\": 1"));
    let v2_effects =
        fs::read_to_string(manifest.join("tests/fixtures/abi_v2/effects.json")).expect("ABI v2");
    assert!(v2_effects.contains("\"generated_abi\": 2"));
    let v3_effects =
        fs::read_to_string(manifest.join("tests/fixtures/abi_v3/effects.json")).expect("ABI v3");
    assert!(v3_effects.contains("\"generated_abi\": 3"));
    let v4_effects =
        fs::read_to_string(manifest.join("tests/fixtures/abi_v4/effects.json")).expect("ABI v4");
    assert!(v4_effects.contains("\"generated_abi\": 4"));
    assert_ne!(
        v1_effects,
        fs::read_to_string(fixture.join("effects.json")).expect("ABI v5")
    );
    assert_ne!(
        v2_effects,
        fs::read_to_string(fixture.join("effects.json")).expect("ABI v5")
    );
    assert_ne!(
        v3_effects,
        fs::read_to_string(fixture.join("effects.json")).expect("ABI v5")
    );
    assert_ne!(
        v4_effects,
        fs::read_to_string(fixture.join("effects.json")).expect("ABI v5")
    );
}
