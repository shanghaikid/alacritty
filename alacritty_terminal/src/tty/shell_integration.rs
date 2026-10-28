//! Private startup trampoline: user shell files stay untouched and ZDOTDIR is restored.
use std::io;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

use super::Options;

pub(super) fn prepare(
    config: &Options,
    default_shell: &str,
    command: &mut Command,
) -> io::Result<Option<TempDir>> {
    let shell = config.shell.as_ref();
    let program = shell.map_or(default_shell, |shell| shell.program.as_str());
    // Do not interfere with -c commands, scripts, clean shells, or arbitrary custom arguments.
    if !config.shell_integration
        || Path::new(program).file_name().is_none_or(|name| name != "zsh")
        || shell.is_some_and(|shell| {
            shell
                .args
                .iter()
                .any(|arg| !matches!(arg.as_str(), "-l" | "-i" | "-il" | "-li" | "--login"))
        })
    {
        return Ok(None);
    }
    let directory = tempfile::Builder::new().prefix("alacritty-zsh-").tempdir()?;
    for (name, source) in [
        (".zshenv", include_str!("../../../extra/shell-integration/zsh/.zshenv")),
        (".zprofile", include_str!("../../../extra/shell-integration/zsh/.zprofile")),
        (".zshrc", include_str!("../../../extra/shell-integration/zsh/.zshrc")),
        ("integration.zsh", include_str!("../../../extra/shell-integration/zsh/integration.zsh")),
    ] {
        std::fs::write(directory.path().join(name), source)?;
    }
    let original = config
        .env
        .get("ZDOTDIR")
        .map(std::ffi::OsString::from)
        .or_else(|| std::env::var_os("ZDOTDIR"));
    command.env("ALACRITTY_ZDOTDIR_SET", if original.is_some() { "1" } else { "0" });
    command.env("ALACRITTY_ZDOTDIR", original.unwrap_or_default());
    command.env("ZDOTDIR", directory.path());
    Ok(Some(directory))
}
