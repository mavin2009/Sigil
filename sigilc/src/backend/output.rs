//! Transactional, concurrency-safe generated-directory replacement.

use anyhow::{bail, Context, Result};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static GENERATION_ID: AtomicU64 = AtomicU64::new(0);

pub struct GeneratedCrate {
    pub lib_rs: String,
    pub main_rs: Option<String>,
    pub cargo_toml: String,
    pub topology_mermaid: Option<String>,
    pub topology_dot: Option<String>,
    pub residual_risk_md: String,
    pub residual_risk_json: String,
    pub effect_contracts_json: String,
    pub build_manifest_json: String,
}

struct RemoveFileOnDrop(PathBuf);

impl Drop for RemoveFileOnDrop {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

struct RemoveDirOnDrop(Option<PathBuf>);

impl Drop for RemoveDirOnDrop {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_dir_all(path);
        }
    }
}

fn sibling_path(parent: &Path, name: &str, role: &str, id: u64) -> PathBuf {
    parent.join(format!(".{name}.sigil-{role}-{}-{id}", std::process::id()))
}

fn write_file(root: &Path, relative: &str, contents: &str) -> Result<()> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating generated directory {}", parent.display()))?;
    }
    fs::write(&path, contents).with_context(|| format!("writing generated file {}", path.display()))
}

/// Replace an entire generated crate as one directory transaction.
///
/// A sibling lock rejects concurrent writers. Files are written to a new
/// sibling directory, the prior output is moved aside, and a same-filesystem
/// rename publishes the complete new tree. Omitted optional files therefore
/// remove obsolete `main.rs` and graph outputs deliberately.
pub fn write_generated_crate(output: &Path, generated: &GeneratedCrate) -> Result<()> {
    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .ok_or_else(|| {
            anyhow::anyhow!(
                "generated output must name a concrete directory, got '{}'",
                output.display()
            )
        })?;
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("creating output parent {}", parent.display()))?;

    let lock_path = parent.join(format!(".{name}.sigil.lock"));
    let mut lock = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .with_context(|| {
            format!(
                "another compiler is generating '{}' (lock {})",
                output.display(),
                lock_path.display()
            )
        })?;
    writeln!(lock, "pid={}", std::process::id()).context("recording generation lock owner")?;
    lock.sync_all().context("syncing generation lock")?;
    let _lock_cleanup = RemoveFileOnDrop(lock_path);

    let id = GENERATION_ID.fetch_add(1, Ordering::Relaxed);
    let staging = sibling_path(parent, name, "tmp", id);
    fs::create_dir(&staging)
        .with_context(|| format!("creating staging directory {}", staging.display()))?;
    let mut staging_cleanup = RemoveDirOnDrop(Some(staging.clone()));

    write_file(&staging, "src/lib.rs", &generated.lib_rs)?;
    if let Some(main) = &generated.main_rs {
        write_file(&staging, "src/main.rs", main)?;
    }
    write_file(&staging, "Cargo.toml", &generated.cargo_toml)?;
    if let Some(mermaid) = &generated.topology_mermaid {
        write_file(&staging, "topology.mmd", mermaid)?;
    }
    if let Some(dot) = &generated.topology_dot {
        write_file(&staging, "topology.dot", dot)?;
    }
    write_file(&staging, "RESIDUAL_RISK.md", &generated.residual_risk_md)?;
    write_file(
        &staging,
        "RESIDUAL_RISK.json",
        &generated.residual_risk_json,
    )?;
    write_file(
        &staging,
        "SIGIL_EFFECTS.json",
        &generated.effect_contracts_json,
    )?;
    write_file(&staging, "SIGIL_BUILD.json", &generated.build_manifest_json)?;

    let backup = sibling_path(parent, name, "backup", id);
    let had_previous = output.exists();
    if had_previous {
        fs::rename(output, &backup).with_context(|| {
            format!(
                "moving existing output '{}' to transaction backup '{}'",
                output.display(),
                backup.display()
            )
        })?;
    }
    if let Err(error) = fs::rename(&staging, output) {
        if had_previous {
            fs::rename(&backup, output).with_context(|| {
                format!(
                    "publishing failed ({error}); restoring previous output '{}'",
                    output.display()
                )
            })?;
        }
        bail!(
            "publishing generated directory '{}' failed: {error}",
            output.display()
        );
    }
    staging_cleanup.0 = None;
    if had_previous {
        fs::remove_dir_all(&backup)
            .with_context(|| format!("removing transaction backup {}", backup.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(label: &str, with_optional: bool) -> GeneratedCrate {
        GeneratedCrate {
            lib_rs: format!("// {label}\n"),
            main_rs: with_optional.then(|| "fn main() {}\n".into()),
            cargo_toml: "[package]\nname=\"fixture\"\nversion=\"0.0.0\"\n".into(),
            topology_mermaid: with_optional.then(|| "flowchart LR\n".into()),
            topology_dot: with_optional.then(|| "digraph x {}\n".into()),
            residual_risk_md: "# risks\n".into(),
            residual_risk_json: "{}\n".into(),
            effect_contracts_json: "{}\n".into(),
            build_manifest_json: "{}\n".into(),
        }
    }

    #[test]
    fn replacement_removes_obsolete_optional_files() {
        let root = std::env::temp_dir().join(format!(
            "sigil-output-test-{}-{}",
            std::process::id(),
            GENERATION_ID.fetch_add(1, Ordering::Relaxed)
        ));
        write_generated_crate(&root, &fixture("first", true)).expect("first generation");
        assert!(root.join("src/main.rs").exists());
        write_generated_crate(&root, &fixture("second", false)).expect("replacement");
        assert_eq!(
            fs::read_to_string(root.join("src/lib.rs")).expect("new library"),
            "// second\n"
        );
        assert!(!root.join("src/main.rs").exists());
        assert!(!root.join("topology.dot").exists());
        fs::remove_dir_all(root).expect("test cleanup");
    }

    #[test]
    fn concurrent_generation_is_rejected_by_lock() {
        let root = std::env::temp_dir().join(format!(
            "sigil-output-lock-test-{}-{}",
            std::process::id(),
            GENERATION_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let parent = root.parent().expect("temporary parent");
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .expect("name");
        let lock = parent.join(format!(".{name}.sigil.lock"));
        fs::write(&lock, "held").expect("create lock");
        let error = write_generated_crate(&root, &fixture("blocked", false))
            .expect_err("concurrent writer must fail")
            .to_string();
        assert!(error.contains("another compiler is generating"), "{error}");
        fs::remove_file(lock).expect("test cleanup");
    }
}
