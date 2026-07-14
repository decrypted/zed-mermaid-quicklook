use anyhow::{anyhow, Context, Result};
use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use wait_timeout::ChildExt;

/// Scale factor for PNG output. Zed's image viewer zooms, so render above display
/// resolution to keep the diagram sharp when zoomed in.
const PNG_SCALE: &str = "6";

/// Wall-clock budget for a single mmdc invocation. mmdc boots Chromium; if that hangs
/// (crashed sandbox, no display, ...) the render thread would otherwise wait forever.
const RENDER_TIMEOUT_SECS: u64 = 30;

/// Disambiguates concurrent staged filenames within one process.
static STAGE_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    let source = strip_empty_comments(source);
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
    fs::write(&input, &source).context("writing diagram source")?;

    // Stage inside the destination directory so the rename below stays on one
    // filesystem, which is what makes it atomic. The pid + counter suffix keeps the
    // staged path unique, so overlapping renders of the same output never collide.
    let staged = dir.join(format!(
        ".{}.tmp.{}.{}.{format}",
        output
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("preview"),
        std::process::id(),
        STAGE_COUNTER.fetch_add(1, Ordering::Relaxed),
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

    eprintln!(
        "render: {} bytes -> {} (mmdc {})",
        source.len(),
        output.display(),
        mmdc.display()
    );

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running mmdc")?;

    let status = match child
        .wait_timeout(Duration::from_secs(RENDER_TIMEOUT_SECS))
        .context("waiting for mmdc")?
    {
        Some(status) => status,
        None => {
            // A hung Chromium never exits on its own; kill and reap it so the render
            // thread is freed rather than blocked forever.
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(&staged);
            return Err(anyhow!("mmdc timed out after {RENDER_TIMEOUT_SECS}s"));
        }
    };

    // The process has exited; drain both pipes so nothing lingers and so error_summary
    // has the diagnostics to work with.
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    if let Some(mut pipe) = child.stdout.take() {
        let mut sink = String::new();
        let _ = pipe.read_to_string(&mut sink);
    }

    if !status.success() {
        let _ = fs::remove_file(&staged);
        let summary = error_summary(&stderr);
        eprintln!("render failed: {summary}");
        return Err(anyhow!("{summary}"));
    }

    fs::rename(&staged, output)
        .with_context(|| format!("moving render into place at {}", output.display()))?;

    Ok(())
}

/// Mermaid's parser rejects a comment marker with nothing after it (`%%` alone on a
/// line), and misreports the position as line 1. People use those as spacers in comment
/// headers, so drop them rather than making the user hunt for it.
fn strip_empty_comments(source: &str) -> String {
    source
        .lines()
        .filter(|line| line.trim() != "%%")
        .collect::<Vec<_>>()
        .join("\n")
}

/// mmdc wraps the real syntax error in Chromium/puppeteer stack noise. Keep the part a
/// user can act on -- the message plus Mermaid's caret and "Expecting ..." detail -- and
/// cut the stack, since this ends up in a Zed toast.
fn error_summary(stderr: &str) -> String {
    let useful: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("Generating"))
        .take_while(|line| !line.starts_with("at "))
        .take(6)
        .collect();

    if useful.is_empty() {
        return "mmdc failed".to_string();
    }

    useful.join("\n")
}

#[cfg(test)]
mod tests {
    use super::strip_empty_comments;

    #[test]
    fn drops_bare_comment_markers_but_keeps_real_comments() {
        let source = "%% header\n%%\n%% more\n\nflowchart TB\n  A-->B\n";
        assert_eq!(
            strip_empty_comments(source),
            "%% header\n%% more\n\nflowchart TB\n  A-->B"
        );
    }

    #[test]
    fn leaves_diagram_body_alone() {
        let source = "flowchart TB\n  %% a note\n  A-->B";
        assert_eq!(strip_empty_comments(source), source);
    }
}
