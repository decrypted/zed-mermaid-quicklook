use zed_extension_api::{self as zed, LanguageServerId, Result};

const LSP_BINARY: &str = "mermaid-quicklook-lsp";
const LSP_PATH_OVERRIDE: &str = "MERMAID_QUICKLOOK_LSP";

struct MermaidQuicklookExtension;

impl zed::Extension for MermaidQuicklookExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        // The LSP inherits the worktree shell environment: it needs PATH to find the
        // `zed` CLI (used to open the preview tab) and `mmdc` (used to render).
        let mut env = worktree.shell_env();

        let command = env
            .iter()
            .find(|(key, _)| key == LSP_PATH_OVERRIDE)
            .map(|(_, value)| value.clone())
            .or_else(|| worktree.which(LSP_BINARY))
            .ok_or_else(|| {
                format!(
                    "`{LSP_BINARY}` not found on PATH. Build and install it with ./scripts/install.sh, \
                     or set {LSP_PATH_OVERRIDE} to its location."
                )
            })?;

        // Render depends on the Mermaid CLI. Resolving it here means the LSP gets an
        // absolute path even when Zed's own PATH differs from the user's shell PATH.
        if let Some(mmdc) = worktree.which("mmdc") {
            env.push(("MMDC_PATH".to_string(), mmdc));
        }

        Ok(zed::Command {
            command,
            args: vec![],
            env,
        })
    }
}

zed::register_extension!(MermaidQuicklookExtension);
