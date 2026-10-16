use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};
use std::result::Result as StdResult;
use std::sync::LazyLock;
use std::{env, fs, io};

use log::{debug, error, info, warn};
use serde::Deserialize;
use serde_yaml::Error as YamlError;
use toml::de::Error as TomlError;
use toml::ser::Error as TomlSeError;
use toml::{Table, Value};

pub mod bell;
pub mod color;
pub mod cursor;
pub mod debug;
pub mod font;
pub mod general;
pub mod monitor;
pub mod scrolling;
pub mod selection;
pub mod serde_utils;
pub mod terminal;
pub mod ui_config;
pub mod window;

mod bindings;
mod mouse;

#[cfg(target_os = "macos")]
pub(crate) use self::bindings::ModsWrapper;
use crate::cli::Options;
#[cfg(test)]
pub use crate::config::bindings::Binding;
pub use crate::config::bindings::{
    Action, BindingKey, BindingMode, KeyBinding, MouseAction, MouseEvent, SearchAction, ViAction,
};
pub use crate::config::ui_config::UiConfig;
use crate::logging::LOG_TARGET_CONFIG;

/// Maximum number of depth for the configuration file imports.
pub const IMPORT_RECURSION_LIMIT: usize = 5;

/// Fork defaults embedded in the executable, beneath imports and user configuration.
static BUILTIN_CONFIG: LazyLock<Value> = LazyLock::new(|| {
    let config =
        toml::from_str(include_str!("defaults.toml")).expect("invalid embedded Alacritty defaults");

    #[cfg(target_os = "macos")]
    let config = serde_utils::merge(
        config,
        toml::from_str(include_str!("defaults-macos.toml"))
            .expect("invalid embedded macOS defaults"),
    );

    config
});

/// Result from config loading.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors occurring during config loading.
#[derive(Debug)]
pub enum Error {
    /// Couldn't read $HOME environment variable.
    ReadingEnvHome(env::VarError),

    /// io error reading file.
    Io(io::Error),

    /// Invalid toml.
    Toml(TomlError),

    /// Failed toml serialization.
    TomlSe(TomlSeError),

    /// Invalid yaml.
    Yaml(YamlError),
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::ReadingEnvHome(err) => err.source(),
            Error::Io(err) => err.source(),
            Error::Toml(err) => err.source(),
            Error::TomlSe(err) => err.source(),
            Error::Yaml(err) => err.source(),
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Error::ReadingEnvHome(err) => {
                write!(f, "Unable to read $HOME environment variable: {err}")
            },
            Error::Io(err) => write!(f, "Error reading config file: {err}"),
            Error::Toml(err) => write!(f, "Config error: {err}"),
            Error::TomlSe(err) => write!(f, "Yaml conversion error: {err}"),
            Error::Yaml(err) => write!(f, "Config error: {err}"),
        }
    }
}

impl From<env::VarError> for Error {
    fn from(val: env::VarError) -> Self {
        Error::ReadingEnvHome(val)
    }
}

impl From<io::Error> for Error {
    fn from(val: io::Error) -> Self {
        Error::Io(val)
    }
}

impl From<TomlError> for Error {
    fn from(val: TomlError) -> Self {
        Error::Toml(val)
    }
}

impl From<TomlSeError> for Error {
    fn from(val: TomlSeError) -> Self {
        Error::TomlSe(val)
    }
}

impl From<YamlError> for Error {
    fn from(val: YamlError) -> Self {
        Error::Yaml(val)
    }
}

/// Load the configuration file.
pub fn load(options: &mut Options) -> UiConfig {
    let config_path = options
        .config_file
        .clone()
        .or_else(|| installed_config("toml"))
        .or_else(|| installed_config("yml"));

    load_config(config_path, options)
}

fn load_config(config_path: Option<PathBuf>, options: &mut Options) -> UiConfig {
    // Load the config using the following fallback behavior:
    //  - Config path + CLI overrides
    //  - CLI overrides
    //  - Default
    let mut config = config_path
        .as_ref()
        .and_then(|config_path| load_from(config_path).ok())
        .unwrap_or_else(|| {
            let mut config = deserialize_ui_config(Value::Table(Table::new()))
                .expect("invalid embedded Alacritty defaults");
            match config_path {
                Some(config_path) => config.config_paths.push(config_path),
                None => info!(target: LOG_TARGET_CONFIG, "No config file found; using default"),
            }
            config
        });

    after_loading(&mut config, options);

    config
}

/// Attempt to reload the configuration file.
pub fn reload(config_path: &Path, options: &mut Options) -> Result<UiConfig> {
    debug!("Reloading configuration file: {config_path:?}");

    // Load config, propagating errors.
    let mut config = load_from(config_path)?;

    after_loading(&mut config, options);

    Ok(config)
}

/// Modifications after the `UiConfig` object is created.
fn after_loading(config: &mut UiConfig, options: &mut Options) {
    // Override config with CLI options.
    options.override_config(config);
}

/// Load configuration file and log errors.
fn load_from(path: &Path) -> Result<UiConfig> {
    match read_config(path) {
        Ok(config) => Ok(config),
        Err(Error::Io(io)) if io.kind() == io::ErrorKind::NotFound => {
            error!(target: LOG_TARGET_CONFIG, "Unable to load config {path:?}: File not found");
            Err(Error::Io(io))
        },
        Err(err) => {
            error!(target: LOG_TARGET_CONFIG, "Unable to load config {path:?}: {err}");
            Err(err)
        },
    }
}

/// Deserialize configuration file from path.
fn read_config(path: &Path) -> Result<UiConfig> {
    let mut config_paths = Vec::new();
    let config_value = parse_config(path, &mut config_paths, IMPORT_RECURSION_LIMIT)?;

    // Deserialize to concrete type.
    let mut config = deserialize_ui_config(config_value)?;
    config.config_paths = config_paths;

    Ok(config)
}

/// Apply built-in defaults once, after imports have been resolved.
fn deserialize_ui_config(config_value: Value) -> Result<UiConfig> {
    Ok(UiConfig::deserialize(serde_utils::merge(BUILTIN_CONFIG.clone(), config_value))?)
}

/// Deserialize all configuration files as generic Value.
fn parse_config(
    path: &Path,
    config_paths: &mut Vec<PathBuf>,
    recursion_limit: usize,
) -> Result<Value> {
    config_paths.push(path.to_owned());

    // Deserialize the configuration file.
    let config = deserialize_config(path, false)?;

    // Merge config with imports.
    let imports = load_imports(&config, path, config_paths, recursion_limit);
    Ok(serde_utils::merge(imports, config))
}

/// Deserialize a configuration file.
pub fn deserialize_config(path: &Path, warn_pruned: bool) -> Result<Value> {
    let mut contents = fs::read_to_string(path)?;

    // Remove UTF-8 BOM.
    if contents.starts_with('\u{FEFF}') {
        contents = contents.split_off(3);
    }

    // Convert YAML to TOML as a transitionary fallback mechanism.
    let extension = path.extension().unwrap_or_default();
    if (extension == "yaml" || extension == "yml") && !contents.trim().is_empty() {
        warn!(
            "YAML config {path:?} is deprecated, please migrate to TOML using `alacritty migrate`"
        );

        let mut value: serde_yaml::Value = serde_yaml::from_str(&contents)?;
        prune_yaml_nulls(&mut value, warn_pruned);
        contents = toml::to_string(&value)?;
    }

    // Load configuration file as Value.
    let config: Value = toml::from_str(&contents)?;

    Ok(config)
}

/// Load all referenced configuration files.
fn load_imports(
    config: &Value,
    base_path: &Path,
    config_paths: &mut Vec<PathBuf>,
    recursion_limit: usize,
) -> Value {
    // Get paths for all imports.
    let import_paths = match imports(config, base_path, recursion_limit) {
        Ok(import_paths) => import_paths,
        Err(err) => {
            error!(target: LOG_TARGET_CONFIG, "{err}");
            return Value::Table(Table::new());
        },
    };

    // Parse configs for all imports recursively.
    let mut merged = Value::Table(Table::new());
    for import_path in import_paths {
        let path = match import_path {
            Ok(path) => path,
            Err(err) => {
                error!(target: LOG_TARGET_CONFIG, "{err}");
                continue;
            },
        };

        match parse_config(&path, config_paths, recursion_limit - 1) {
            Ok(config) => merged = serde_utils::merge(merged, config),
            Err(Error::Io(io)) if io.kind() == io::ErrorKind::NotFound => {
                info!(target: LOG_TARGET_CONFIG, "Config import not found:\n  {:?}", path.display());
                continue;
            },
            Err(err) => {
                error!(target: LOG_TARGET_CONFIG, "Unable to import config {path:?}: {err}")
            },
        }
    }

    merged
}

/// Get all import paths for a configuration.
pub fn imports(
    config: &Value,
    base_path: &Path,
    recursion_limit: usize,
) -> StdResult<Vec<StdResult<PathBuf, String>>, String> {
    let imports =
        config.get("import").or_else(|| config.get("general").and_then(|g| g.get("import")));
    let imports = match imports {
        Some(Value::Array(imports)) => imports,
        Some(_) => return Err("Invalid import type: expected a sequence".into()),
        None => return Ok(Vec::new()),
    };

    // Limit recursion to prevent infinite loops.
    if !imports.is_empty() && recursion_limit == 0 {
        return Err("Exceeded maximum configuration import depth".into());
    }

    let mut import_paths = Vec::new();

    for import in imports {
        let path = match import {
            Value::String(path) => PathBuf::from(path),
            _ => {
                import_paths.push(Err("Invalid import element type: expected path string".into()));
                continue;
            },
        };

        let normalized = normalize_import(base_path, path);

        import_paths.push(Ok(normalized));
    }

    Ok(import_paths)
}

/// Normalize import paths.
pub fn normalize_import(base_config_path: &Path, import_path: impl Into<PathBuf>) -> PathBuf {
    let mut import_path = import_path.into();

    // Resolve paths relative to user's home directory.
    if let (Ok(stripped), Some(home_dir)) = (import_path.strip_prefix("~/"), home::home_dir()) {
        import_path = home_dir.join(stripped);
    }

    if import_path.is_relative() {
        if let Some(base_config_dir) = base_config_path.parent() {
            import_path = base_config_dir.join(import_path)
        }
    }

    import_path
}

/// Prune the nulls from the YAML to ensure TOML compatibility.
fn prune_yaml_nulls(value: &mut serde_yaml::Value, warn_pruned: bool) {
    fn walk(value: &mut serde_yaml::Value, warn_pruned: bool) -> bool {
        match value {
            serde_yaml::Value::Sequence(sequence) => {
                sequence.retain_mut(|value| !walk(value, warn_pruned));
                sequence.is_empty()
            },
            serde_yaml::Value::Mapping(mapping) => {
                mapping.retain(|key, value| {
                    let retain = !walk(value, warn_pruned);
                    if let Some(key_name) = key.as_str().filter(|_| !retain && warn_pruned) {
                        eprintln!("Removing null key \"{key_name}\" from the end config");
                    }
                    retain
                });
                mapping.is_empty()
            },
            serde_yaml::Value::Null => true,
            _ => false,
        }
    }

    if walk(value, warn_pruned) {
        // When the value itself is null return the mapping.
        *value = serde_yaml::Value::Mapping(Default::default());
    }
}

/// Get the location of the first found default config file paths
/// according to the following order:
///
/// 1. $XDG_CONFIG_HOME/alacritty/alacritty.toml
/// 2. $XDG_CONFIG_HOME/alacritty.toml
/// 3. $HOME/.config/alacritty/alacritty.toml
/// 4. $HOME/.alacritty.toml
/// 5. /etc/alacritty/alacritty.toml
#[cfg(not(windows))]
pub fn installed_config(suffix: &str) -> Option<PathBuf> {
    let file_name = format!("alacritty.{suffix}");

    // Try using XDG location by default.
    xdg::BaseDirectories::with_prefix("alacritty")
        .find_config_file(&file_name)
        .or_else(|| xdg::BaseDirectories::new().find_config_file(&file_name))
        .or_else(|| {
            if let Ok(home) = env::var("HOME") {
                // Fallback path: $HOME/.config/alacritty/alacritty.toml.
                let fallback = PathBuf::from(&home).join(".config/alacritty").join(&file_name);
                if fallback.exists() {
                    return Some(fallback);
                }
                // Fallback path: $HOME/.alacritty.toml.
                let hidden_name = format!(".{file_name}");
                let fallback = PathBuf::from(&home).join(hidden_name);
                if fallback.exists() {
                    return Some(fallback);
                }
            }

            let fallback = PathBuf::from("/etc/alacritty").join(&file_name);
            fallback.exists().then_some(fallback)
        })
}

#[cfg(windows)]
pub fn installed_config(suffix: &str) -> Option<PathBuf> {
    let file_name = format!("alacritty.{suffix}");
    dirs::config_dir().map(|path| path.join("alacritty").join(file_name)).filter(|new| new.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_user_config_uses_embedded_defaults() {
        let config = load_config(None, &mut Options::default());
        assert_eq!(config.window.padding(1.), (12., 24.));
        assert!(config.window.dynamic_padding);
        assert!(config.window.maximized());
        assert_eq!(config.scrolling.history(), 100_000);
        assert!(config.selection.save_to_clipboard);
        assert!(config.mouse.hide_when_typing);
        assert!(config.config_paths.is_empty());
        assert!(config.pty_config().shell.is_none());

        let value = toml::Value::try_from(&config).unwrap();
        assert_eq!(value["colors"]["primary"]["background"].as_str(), Some("#1e1e2e"));
        assert_eq!(value["font"]["size"].as_float(), Some(11.));
        #[cfg(target_os = "macos")]
        {
            assert_eq!(config.font.normal().family, "JetBrainsMono Nerd Font Mono");
            assert_eq!(value["window"]["decorations"].as_str(), Some("Transparent"));
            assert_eq!(value["window"]["option_as_alt"].as_str(), Some("Both"));
            assert!(config.global_hotkey().enabled);
            assert_eq!(value["keyboard"]["global_hotkey"]["key"].as_str(), Some("KeyL"));
            assert_eq!(value["keyboard"]["global_hotkey"]["mods"].as_str(), Some("Command|Shift"));
        }
    }

    #[test]
    fn user_imports_reload_and_cli_override_embedded_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let imported = directory.path().join("theme.toml");
        let path = directory.path().join("alacritty.toml");
        fs::write(&imported, "[font]\nsize = 13.0\n[window.padding]\nx = 20\n").unwrap();
        fs::write(&path, "[general]\nimport = ['theme.toml']\n[window.padding]\nx = 30\n").unwrap();

        let mut options = Options::default();
        options.config_options =
            crate::cli::ParsedOptions::from_options(&["font.size=14.0".into()]);
        let config = load_config(Some(path.clone()), &mut options);
        assert_eq!(config.window.padding(1.), (30., 24.));
        assert_eq!(config.font.size(), crossfont::Size::new(14.));
        assert_eq!(config.config_paths, vec![path.clone(), imported]);

        fs::write(&path, "[window.padding]\nx = 40\n").unwrap();
        let config = reload(&path, &mut Options::default()).unwrap();
        assert_eq!(config.window.padding(1.), (40., 24.));
        assert_eq!(config.font.size(), crossfont::Size::new(11.));
        assert_eq!(config.config_paths, vec![path.clone()]);

        fs::remove_file(&path).unwrap();
        let config = load_config(Some(path.clone()), &mut Options::default());
        assert_eq!(config.window.padding(1.), (12., 24.));
        assert_eq!(config.config_paths, vec![path]);
    }

    #[test]
    fn embedded_defaults_leave_binding_overrides_to_the_binding_deserializer() {
        // Config merges concatenate arrays. Embedded keyboard bindings would execute alongside
        // matching user bindings, instead of being replaced like Rust's default bindings.
        assert!(
            BUILTIN_CONFIG.get("keyboard").and_then(|keyboard| keyboard.get("bindings")).is_none()
        );

        let value = toml::from_str(
            r#"[keyboard]
            bindings = [{ key = "L", mods = "Control", action = "None" }]"#,
        )
        .unwrap();
        let config = deserialize_ui_config(value).unwrap();
        let key = BindingKey::Keycode {
            key: winit::keyboard::Key::Character("l".into()),
            location: bindings::KeyLocation::Standard,
        };
        let matching: Vec<_> = config
            .key_bindings()
            .iter()
            .filter(|binding| {
                binding.is_triggered_by(
                    BindingMode::empty(),
                    winit::keyboard::ModifiersState::CONTROL,
                    &key,
                )
            })
            .collect();
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].action, Action::None);
    }

    #[test]
    fn empty_config() {
        toml::from_str::<UiConfig>("").unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn global_hotkey_can_be_overridden_disabled_and_reloaded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("alacritty.toml");
        fs::write(&path, "[keyboard.global_hotkey]\nkey = 'F12'\nmods = 'Control|Alt'\n").unwrap();
        let config = load_config(Some(path.clone()), &mut Options::default());
        let hotkey = config.global_hotkey();
        assert!(hotkey.enabled);
        assert_eq!(hotkey.key, winit::keyboard::KeyCode::F12);
        assert_eq!(
            hotkey.mods.0,
            winit::keyboard::ModifiersState::CONTROL | winit::keyboard::ModifiersState::ALT
        );

        fs::write(&path, "[keyboard.global_hotkey]\nenabled = false\n").unwrap();
        assert!(!reload(&path, &mut Options::default()).unwrap().global_hotkey().enabled);

        fs::write(&path, "").unwrap();
        let config = reload(&path, &mut Options::default()).unwrap();
        assert!(config.global_hotkey().enabled);
        assert_eq!(config.global_hotkey().key, winit::keyboard::KeyCode::KeyL);

        let mut options = Options::default();
        options.config_options = crate::cli::ParsedOptions::from_options(&[
            "keyboard.global_hotkey.enabled=false".into(),
        ]);
        assert!(!load_config(Some(path), &mut options).global_hotkey().enabled);
    }

    fn yaml_to_toml(contents: &str) -> String {
        let mut value: serde_yaml::Value = serde_yaml::from_str(contents).unwrap();
        prune_yaml_nulls(&mut value, false);
        toml::to_string(&value).unwrap()
    }

    #[test]
    fn yaml_with_nulls() {
        let contents = r#"
        window:
            blinking: Always
            cursor:
            not_blinking: Always
            some_array:
              - { window: }
              - { window: "Hello" }

        "#;
        let toml = yaml_to_toml(contents);
        assert_eq!(
            toml.trim(),
            r#"[window]
blinking = "Always"
not_blinking = "Always"

[[window.some_array]]
window = "Hello""#
        );
    }

    #[test]
    fn empty_yaml_to_toml() {
        let contents = r#"

        "#;
        let toml = yaml_to_toml(contents);
        assert!(toml.is_empty());
    }
}
