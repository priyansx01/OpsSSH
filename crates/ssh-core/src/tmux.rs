//! Dedicated tmux socket and validated per-tab session identities.
#[derive(Debug, Clone)]
pub struct TmuxOptions {
    pub session_name: String,
}
impl TmuxOptions {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.session_name.is_empty()
                && self.session_name.len() <= 100
                && self
                    .session_name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "tmux session name must contain only letters, digits, underscores and hyphens"
        );
        Ok(())
    }
    pub fn attach_command(&self) -> anyhow::Result<String> {
        self.validate()?;
        Ok(format!(
            r#"command -v tmux >/dev/null 2>&1 || {{ printf 'tmux is not installed; disable session persistence or install tmux.\r\n'; exit 127; }}
umask 077
mkdir -p "$HOME/.config/opsssh" || exit 1
cfg="$HOME/.config/opsssh/tmux.conf"
if [ ! -e "$cfg" ]; then
(set -C; cat > "$cfg" <<'OPSSSH_TMUX_CONFIG'
source-file -q ~/.tmux.conf
set -g history-limit 50000
set -g mouse off
set -g status off
set -sg escape-time 0
set -g focus-events on
set -g set-clipboard on
set -g default-terminal tmux-256color
set -qg extended-keys on
set -qg extended-keys-format csi-u
set -qas terminal-features 'xterm*:RGB:clipboard:extkeys:focus:hyperlinks:sync'
OPSSSH_TMUX_CONFIG
) || exit 1
fi
tmux -L opsssh capture-pane -p -e -S -50000 -t '{}' 2>/dev/null || :
exec tmux -L opsssh -f "$cfg" new-session -A -s '{}'"#,
            self.session_name, self.session_name
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_shell_injection() {
        assert!(
            TmuxOptions {
                session_name: "work'; touch /tmp/x".into()
            }
            .attach_command()
            .is_err()
        );
    }
    #[test]
    fn isolates_socket() {
        assert!(
            TmuxOptions {
                session_name: "server_tab-123".into()
            }
            .attach_command()
            .unwrap()
            .contains("-L opsssh")
        );
    }
}
