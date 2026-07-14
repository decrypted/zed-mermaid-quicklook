use anyhow::{anyhow, Context, Result};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Scale factor for PNG output. Zed's image viewer zooms, so render above display
/// resolution to keep the diagram sharp when zoomed in.
const PNG_SCALE: &str = "3";

pub fn mmdc_path() -> Result<PathBuf> {
    if let Ok(path) = env::var("MMDC_PATH") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
    }

    which::which("mmdc").map_err(|_| {
        anyhow!("mmdc not found. Install it with: npm install -g @mermaid-js/mermaid-cli")
    })
}

/// Render Mermaid source to `output`, whose extension selects the format (png or svg).
///
/// The file is replaced atomically. Zed watches the preview file and repaints on
/// change, so a partially written file would otherwise flash a corrupt image.
pub fn render(source: &str, output: &Path) -> Result<()> {
    if source.trim().is_empty() {
        return Err(anyhow!("diagram is empty"));
    }

    let mmdc = mmdc_path()?;
    let format = output
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("png");

    let dir = output
        .parent()
        .ok_or_else(|| anyhow!("output path has no parent directory"))?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let scratch = tempfile::tempdir().context("creating temp dir")?;
    let input = scratch.path().join("diagram.mmd");
    fs::write(&input, source).context("writing diagram source")?;

    // Stage inside the destination directory so the rename below stays on one
    // filesystem, which is what makes it atomic.
    let staged = dir.join(format!(
        ".{}.tmp.{format}",
        output
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("preview")
    ));

    let mut command = Command::new(&mmdc);
    command
        .arg("-i")
        .arg(&input)
        .arg("-o")
        .arg(&staged)
        .arg("-b")
        .arg("white");

    if format == "png" {
        command.arg("-s").arg(PNG_SCALE);
    }

    let result = command.output().context("running mmdc")?;

    if !result.status.success() {
        let _ = fs::remove_file(&staged);
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(anyhow!("{}", first_useful_line(&stderr)));
    }

    fs::rename(&staged, output)
        .with_context(|| format!("moving render into place at {}", output.display()))?;

    Ok(())
}

/// mmdc prints Chromium/puppeteer noise around the actual syntax error. Surface the
/// part a user can act on, since this ends up in a Zed toast.
fn first_useful_line(stderr: &str) -> String {
    let error = stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("Generating"))
        .unwrap_or("mmdc failed");

    error.to_string()
}
