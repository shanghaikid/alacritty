use serde::{Deserialize, Deserializer, Serialize, de};
use toml::Value;

use alacritty_config_derive::{ConfigDeserialize, SerdeReplace};
use alacritty_terminal::term::Osc52;

use crate::config::ui_config::{Program, StringVisitor};

#[derive(ConfigDeserialize, Serialize, Clone, Debug, PartialEq)]
pub struct Terminal {
    /// OSC52 support mode.
    pub osc52: SerdeOsc52,
    /// Path to a shell program to run on startup.
    pub shell: Option<Program>,
    /// Assemble Unicode graphemes; requires a cluster-capable renderer.
    pub grapheme_clusters: bool,
    /// Automatically load OSC 133 hooks in compatible interactive zsh sessions.
    pub shell_integration: bool,
    /// Notify when a long command completes in an unfocused terminal.
    pub notify_on_command_finish: bool,
    /// Minimum runtime in seconds (at least one second).
    pub command_finish_threshold: u32,
}

#[derive(SerdeReplace, Serialize, Default, Copy, Clone, Debug, PartialEq)]
pub struct SerdeOsc52(pub Osc52);

impl<'de> Deserialize<'de> for SerdeOsc52 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = deserializer.deserialize_str(StringVisitor)?;
        Osc52::deserialize(Value::String(value)).map(SerdeOsc52).map_err(de::Error::custom)
    }
}

impl Default for Terminal {
    fn default() -> Self {
        Self {
            osc52: Default::default(),
            shell: None,
            grapheme_clusters: false,
            shell_integration: true,
            notify_on_command_finish: true,
            command_finish_threshold: 10,
        }
    }
}

impl Terminal {
    pub fn should_notify(&self, focused: bool, elapsed: std::time::Duration) -> bool {
        self.notify_on_command_finish
            && !focused
            && elapsed.as_secs() >= u64::from(self.command_finish_threshold.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn completion_notifications_require_background_and_duration() {
        let mut config = Terminal::default();
        assert!(!config.should_notify(true, Duration::from_secs(20)));
        assert!(!config.should_notify(false, Duration::from_secs(9)));
        assert!(config.should_notify(false, Duration::from_secs(10)));
        config.notify_on_command_finish = false;
        assert!(!config.should_notify(false, Duration::from_secs(20)));
        config.notify_on_command_finish = true;
        config.command_finish_threshold = 0;
        assert!(!config.should_notify(false, Duration::from_millis(999)));
    }
}
