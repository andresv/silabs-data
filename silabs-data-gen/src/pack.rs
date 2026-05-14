use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub fn extract_pack(pack: &Path, dst: &Path) -> Result<()> {
    let file = std::fs::File::open(pack).with_context(|| format!("opening {}", pack.display()))?;
    let mut zip = zip::ZipArchive::new(file)?;
    std::fs::create_dir_all(dst)?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let out = dst.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
        } else {
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut sink = std::fs::File::create(&out)?;
            std::io::copy(&mut entry, &mut sink)?;
        }
    }
    Ok(())
}

pub fn find_pdsc(extract_dir: &Path) -> Result<PathBuf> {
    let mut hits = Vec::new();
    for entry in std::fs::read_dir(extract_dir)? {
        let entry = entry?;
        if entry.path().extension().is_some_and(|e| e == "pdsc") {
            hits.push(entry.path());
        }
    }
    match hits.len() {
        1 => Ok(hits.pop().unwrap()),
        0 => bail!("no .pdsc found in {}", extract_dir.display()),
        n => bail!("expected 1 .pdsc in {}, found {}", extract_dir.display(), n),
    }
}
