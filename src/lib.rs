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
        // The full shell environment carries secrets (AWS keys, tokens, ...) that the
        // LSP would in turn hand to mmdc/Chromium and the `zed` CLI. Forward only the
        // variables those subprocesses actually need.
        let shell_env = worktree.shell_env();

        let command = shell_env
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

        // Allowlist: PATH/HOME to locate and run the tools, plus the display/session
        // handles the `zed` GUI and Chromium need to reach the user's desktop.
        const ALLOWED: &[&str] = &[
            "PATH",
            "HOME",
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "XDG_RUNTIME_DIR",
            "DBUS_SESSION_BUS_ADDRESS",
            "XDG_DATA_HOME",
            "XDG_CONFIG_HOME",
            "LANG",
            "LC_ALL",
            "TERM",
            "MMDC_PATH",
        ];

        let mut env: Vec<(String, String)> = shell_env
            .iter()
            .filter(|(key, _)| ALLOWED.contains(&key.as_str()))
            .cloned()
            .collect();

        // Render depends on the Mermaid CLI. Resolving it here means the LSP gets an
        // absolute path even when Zed's own PATH differs from the user's shell PATH.
        if let Some(mmdc) = worktree.which("mmdc") {
            env.retain(|(key, _)| key != "MMDC_PATH");
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
