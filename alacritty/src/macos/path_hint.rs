//! Opening local terminal paths without interpreting them as shell commands.

use std::ffi::OsString;
use std::io::{self, Read, Seek};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use std::{mem, ptr, thread};

use log::warn;

use super::proc;

/// Resolve and open off the event thread: neither tmux nor LaunchServices may block rendering.
pub fn open(text: String, pid: i32) {
    thread::spawn(move || {
        let result = (|| {
            let cwd = if Path::new(&text).is_absolute() || text.starts_with("~/") {
                PathBuf::new()
            } else {
                working_directory(pid)?
            };
            let path = resolve_path(&text, &cwd, home::home_dir().as_deref())?;
            let status = Command::new("/usr/bin/open")
                .arg(path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
            if !status.success() {
                return Err(io::Error::other(format!("open exited with {status}")));
            }
            Ok(())
        })();
        if let Err(err) = result {
            warn!("Unable to open local path {text:?}: {err}");
        }
    });
}

fn resolve_path(text: &str, cwd: &Path, home: Option<&Path>) -> io::Result<PathBuf> {
    let path = if let Some(relative) = text.strip_prefix("~/") {
        home.ok_or_else(|| io::Error::other("home directory unavailable"))?.join(relative)
    } else {
        cwd.join(text)
    };
    // Require an existing local file/directory and pass an absolute argument to open.
    // In particular, strings beginning with '-' cannot become command options.
    path.canonicalize()
}

fn working_directory(pid: i32) -> io::Result<PathBuf> {
    let (executable, args) = process_command(pid)?;
    if executable.file_name().is_some_and(|name| name == "tmux") {
        let options = tmux_socket_options(&args);
        let mut command = Command::new(executable);
        // -N prevents inadvertently starting a server. Match this terminal's client,
        // rather than whichever attached session tmux happens to select by default.
        command.args(options).args(["-N", "list-clients", "-F"]);
        command.arg("#{client_pid}\t#{pane_current_path}");
        let output = query(&mut command)?;
        return client_path(&output, pid)
            .ok_or_else(|| io::Error::other("tmux client working directory unavailable"));
    }
    proc::cwd(pid).map_err(io::Error::other)
}

fn client_path(output: &str, pid: i32) -> Option<PathBuf> {
    output.lines().find_map(|line| {
        let (client, path) = line.split_once('\t')?;
        (client.parse::<i32>().ok() == Some(pid) && Path::new(path).is_absolute())
            .then(|| PathBuf::from(path))
    })
}

/// Preserve explicit named servers/custom sockets, stopping before the tmux subcommand.
fn tmux_socket_options(args: &[OsString]) -> Vec<OsString> {
    let mut options = Vec::new();
    let mut args = args.iter().skip(1);
    while let Some(arg) = args.next() {
        let bytes = arg.as_bytes();
        if bytes == b"--" || !bytes.starts_with(b"-") {
            break;
        }
        if bytes == b"-L" || bytes == b"-S" {
            if let Some(value) = args.next() {
                options.extend([arg.clone(), value.clone()]);
            }
        } else if bytes.starts_with(b"-L") || bytes.starts_with(b"-S") {
            options.push(arg.clone());
        } else if [b"-f".as_slice(), b"-T", b"-c"].contains(&bytes) {
            args.next();
        }
    }
    options
}

/// Use a file for stdout so a full pipe cannot deadlock the bounded wait.
fn query(command: &mut Command) -> io::Result<String> {
    let mut output = tempfile::tempfile()?;
    let mut child =
        command.stdin(Stdio::null()).stdout(output.try_clone()?).stderr(Stdio::null()).spawn()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(status)) => return Err(io::Error::other(format!("tmux exited with {status}"))),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(result.err().unwrap_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "tmux path lookup timed out")
                }));
            },
        }
    }
    output.rewind()?;
    let mut text = String::new();
    output.take(1024 * 1024).read_to_string(&mut text)?;
    Ok(text)
}

/// Read only the executable and argv; environment variables are never returned.
fn process_command(pid: i32) -> io::Result<(PathBuf, Vec<OsString>)> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size = 0;
    unsafe {
        if libc::sysctl(mib.as_mut_ptr(), 3, ptr::null_mut(), &mut size, ptr::null_mut(), 0) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    if size > 1024 * 1024 {
        return Err(io::Error::other("process arguments too large"));
    }
    let mut buffer = vec![0; size];
    unsafe {
        if libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buffer.as_mut_ptr().cast(),
            &mut size,
            ptr::null_mut(),
            0,
        ) != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    buffer.truncate(size);
    parse_command(&buffer).ok_or_else(|| io::Error::other("invalid process arguments"))
}

fn parse_command(buffer: &[u8]) -> Option<(PathBuf, Vec<OsString>)> {
    let argc = i32::from_ne_bytes(buffer.get(..mem::size_of::<i32>())?.try_into().ok()?);
    if !(1..=65536).contains(&argc) {
        return None;
    }
    let buffer = buffer.get(mem::size_of::<i32>()..)?;
    let end = buffer.iter().position(|&b| b == 0)?;
    let executable = PathBuf::from(OsString::from_vec(buffer[..end].to_vec()));
    let argv_start = buffer[end..].iter().position(|&b| b != 0)? + end;
    let args = buffer[argv_start..]
        .split(|&b| b == 0)
        .take(argc as usize)
        .map(|arg| OsString::from_vec(arg.to_vec()))
        .collect::<Vec<_>>();
    (args.len() == argc as usize).then_some((executable, args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_existing_paths_without_shell_expansion() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(".codex-artifacts/架构图");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("index.html");
        std::fs::write(&file, "test").unwrap();
        let expected = file.canonicalize().unwrap();
        for path in [
            ".codex-artifacts/架构图/index.html",
            "./.codex-artifacts/架构图/index.html",
            "~/.codex-artifacts/架构图/index.html",
            file.to_str().unwrap(),
        ] {
            assert_eq!(resolve_path(path, root.path(), Some(root.path())).unwrap(), expected);
        }
        assert_eq!(resolve_path("../架构图/index.html", &dir, None).unwrap(), expected);
        assert!(resolve_path("missing.html", root.path(), None).is_err());
        assert!(resolve_path("$HOME/index.html", root.path(), None).is_err());
        let literal = root.path().join("-$(touch nope).html");
        std::fs::write(&literal, "test").unwrap();
        assert_eq!(
            resolve_path("-$(touch nope).html", root.path(), None).unwrap(),
            literal.canonicalize().unwrap()
        );
    }

    #[test]
    fn selects_the_matching_tmux_client() {
        let output = "123\t/tmp/wrong\n456\t/Users/test/project with spaces\n";
        assert_eq!(client_path(output, 456), Some("/Users/test/project with spaces".into()));
        assert_eq!(client_path(output, 789), None);
        assert_eq!(client_path("456\t\n", 456), None);
    }

    #[test]
    fn parses_argv_without_environment() {
        let mut buffer = 5_i32.to_ne_bytes().to_vec();
        buffer.extend_from_slice(b"/opt/bin/tmux\0\0tmux\0-L\0test\0attach\0\0SECRET=value\0");
        let (exe, args) = parse_command(&buffer).unwrap();
        assert_eq!(exe, Path::new("/opt/bin/tmux"));
        assert_eq!(args, ["tmux", "-L", "test", "attach", ""].map(OsString::from));
        assert_eq!(tmux_socket_options(&args), ["-L", "test"].map(OsString::from));
        let args = ["tmux", "-f", "config", "-S/tmp/socket", "new", "-Lother"].map(OsString::from);
        assert_eq!(tmux_socket_options(&args), ["-S/tmp/socket"].map(OsString::from));
    }

    #[test]
    fn reads_current_process() {
        let (exe, _) = process_command(std::process::id() as i32).unwrap();
        assert_eq!(
            exe.canonicalize().unwrap(),
            std::env::current_exe().unwrap().canonicalize().unwrap()
        );
        assert_eq!(
            working_directory(std::process::id() as i32).unwrap(),
            std::env::current_dir().unwrap()
        );
    }

    #[test]
    #[ignore = "requires a disposable attached tmux client"]
    fn attached_tmux_client_directory() {
        let pid = std::env::var("ALACRITTY_TEST_TMUX_PID").unwrap().parse().unwrap();
        let expected = PathBuf::from(std::env::var_os("ALACRITTY_TEST_TMUX_CWD").unwrap());
        assert_eq!(
            working_directory(pid).unwrap().canonicalize().unwrap(),
            expected.canonicalize().unwrap()
        );
    }

    #[test]
    fn stalled_query_is_terminated() {
        let start = Instant::now();
        let error = query(Command::new("/bin/sleep").arg("10")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
