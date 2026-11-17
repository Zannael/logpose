//! Which program "Open" starts for a project (DEC-005, OPEN-002).
//!
//! Python projects open in PyCharm, other code projects in WebStorm, and anything else opens the
//! directory with the system opener. Programs come from config (`[open]`) and are looked up on
//! `PATH`; a missing editor falls back to opening the directory.
//!
//! macOS and Windows (XP-002): editors not on `PATH` are also found as app bundles (`open -a`) or
//! JetBrains Toolbox scripts; "Launch agent here" uses Terminal.app (via `osascript`) on macOS and
//! Windows Terminal (`wt`) or a new console window on Windows. Every OS is built from a
//! [`Platform`], so each is unit-tested on any machine.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::OpenConfig;
use crate::platform::{Os, Platform};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectKind {
    Python,
    Code,
    Other,
}

const PYTHON_MARKERS: &[&str] = &[
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "requirements.txt",
    "Pipfile",
    "environment.yml",
    "poetry.lock",
];
const CODE_MARKERS: &[&str] = &[
    "package.json",
    "tsconfig.json",
    "Cargo.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "CMakeLists.txt",
    "Makefile",
    "meson.build",
    "composer.json",
    "Gemfile",
    "deno.json",
];
const CODE_EXTENSIONS: &[&str] = &[
    "js", "jsx", "ts", "tsx", "mjs", "vue", "svelte", "html", "css", "rs", "go", "c", "h", "cpp", "hpp",
    "java", "kt", "swift", "rb", "php", "cs", "sh", "lua",
];

/// Classify by top-level markers, then by top-level source files. Python markers win in mixed projects.
pub fn detect_kind(project: &Path) -> ProjectKind {
    let has = |name: &&str| project.join(name).exists();
    if PYTHON_MARKERS.iter().any(has) {
        return ProjectKind::Python;
    }
    if CODE_MARKERS.iter().any(has) {
        return ProjectKind::Code;
    }
    let (mut py, mut code) = (0usize, 0usize);
    for dir in [project.to_path_buf(), project.join("src")] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match path.extension().and_then(|e| e.to_str()) {
                Some("py" | "ipynb") => py += 1,
                Some(ext) if CODE_EXTENSIONS.contains(&ext) => code += 1,
                _ => {}
            }
        }
    }
    match (py, code) {
        (0, 0) => ProjectKind::Other,
        (p, c) if p >= c => ProjectKind::Python,
        _ => ProjectKind::Code,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpenCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub kind: ProjectKind,
    /// True when the preferred editor was not found and the directory opener is used instead.
    pub fell_back: bool,
    /// Windows: start in a new console window with inherited stdio (the terminal fallback).
    #[serde(skip)]
    pub new_console: bool,
}

impl OpenCommand {
    fn new(program: PathBuf, args: Vec<OsString>, kind: ProjectKind, fell_back: bool) -> Self {
        Self {
            program,
            args,
            kind,
            fell_back,
            new_console: false,
        }
    }

    /// What the user sees ("webstorm", "PyCharm" for `open -a …/PyCharm.app`, "explorer").
    pub fn label(&self) -> String {
        let stem = |p: &Path| {
            p.file_stem()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let name = |p: &Path| {
            // `file_stem` only splits on this OS's separator; Windows paths are tested on Linux too.
            let s = p.to_string_lossy();
            let last = s.rsplit(['/', '\\']).next().unwrap_or_default().to_owned();
            stem(Path::new(&last))
        };
        if name(&self.program) == "open"
            && self.args.first().is_some_and(|a| a == "-a")
            && let Some(app) = self.args.get(1)
        {
            return name(Path::new(app));
        }
        name(&self.program)
    }

    /// The terminal an [`agent_terminal`] command opens ("Terminal", "wt", "a new console window").
    pub fn terminal_label(&self) -> String {
        match self.label().as_str() {
            _ if self.new_console => "a new console window".into(),
            "osascript" => "Terminal".into(),
            other => other.into(),
        }
    }
}

/// Terminals tried in order for "Launch agent here" on Linux when none is configured.
pub const TERMINALS: &[&str] = &[
    "x-terminal-emulator",
    "gnome-terminal",
    "kgx",
    "ptyxis",
    "konsole",
    "kitty",
    "alacritty",
    "wezterm",
    "xfce4-terminal",
    "tilix",
    "foot",
    "xterm",
];

/// Command that opens `terminal` in `dir` running `program args…` (LATER-002). The argument style
/// depends on the terminal's file name; unknown ones get the common `-e program args…`. The child's
/// working directory is also set to `dir` by [`spawn_detached_in`].
pub fn terminal_command(terminal: &Path, dir: &Path, program: &Path, args: &[OsString]) -> Vec<OsString> {
    let name = terminal
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir_s = dir.as_os_str().to_owned();
    let with = |flag: &str| {
        let mut f = OsString::from(flag);
        f.push(&dir_s);
        f
    };
    let mut out: Vec<OsString> = match name.as_str() {
        // x-terminal-emulator is usually gnome-terminal's wrapper on Debian/Ubuntu/Pop!_OS.
        "gnome-terminal" | "gnome-terminal.wrapper" | "x-terminal-emulator" | "ptyxis" | "kgx" => {
            vec![with("--working-directory="), "--".into()]
        }
        "konsole" => vec!["--workdir".into(), dir_s.clone(), "-e".into()],
        "kitty" => vec![with("--directory=")],
        "alacritty" => vec!["--working-directory".into(), dir_s.clone(), "-e".into()],
        "wezterm" => vec!["start".into(), "--cwd".into(), dir_s.clone(), "--".into()],
        "xfce4-terminal" | "tilix" => vec![with("--working-directory="), "-x".into()],
        "foot" => vec![with("--working-directory=")],
        _ => vec!["-e".into()],
    };
    out.push(program.as_os_str().to_owned());
    out.extend(args.iter().cloned());
    out
}

/// The terminal to use: `configured` (name or path) or the first of [`TERMINALS`] that `find` resolves.
pub fn find_terminal(configured: Option<&str>, find: &dyn Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    match configured.filter(|t| !t.is_empty()) {
        Some(t) => find(t),
        None => TERMINALS.iter().find_map(|t| find(t)),
    }
}

/// How "Launch agent here" opens a terminal on this OS (XP-002): the command to spawn, run with
/// [`spawn_detached_in`] in `dir`. `None` when no terminal is found (or the configured one is missing).
///
/// - Linux: `configured` or the first of [`TERMINALS`], with [`terminal_command`].
/// - macOS: a configured emulator on `PATH` (kitty, alacritty, wezterm … via [`terminal_command`]);
///   otherwise (or `"Terminal"`) Terminal.app via `osascript`, see [`macos_terminal_script`].
/// - Windows: `configured` or `wt` (Windows Terminal: `wt -d DIR prog args`, `;` escaped as `\;`);
///   otherwise (or `"cmd"`) the agent itself in a new console window. Newlines in arguments are
///   flattened to ` / ` because console command lines and batch shims (`codex.cmd`) cannot carry them.
pub fn agent_terminal(
    platform: &Platform,
    configured: Option<&str>,
    find: &dyn Fn(&str) -> Option<PathBuf>,
    dir: &Path,
    program: &Path,
    args: &[OsString],
) -> Option<OpenCommand> {
    let configured = configured.filter(|t| !t.is_empty());
    let kind = ProjectKind::Other;
    match platform.os {
        Os::Linux => {
            let terminal = find_terminal(configured, find)?;
            let args = terminal_command(&terminal, dir, program, args);
            Some(OpenCommand::new(terminal, args, kind, false))
        }
        Os::MacOs => match configured {
            Some(t) if !t.eq_ignore_ascii_case("terminal") && !t.eq_ignore_ascii_case("terminal.app") => {
                let terminal = find(t)?;
                let args = terminal_command(&terminal, dir, program, args);
                Some(OpenCommand::new(terminal, args, kind, false))
            }
            _ => {
                let osascript = find("osascript").unwrap_or_else(|| "/usr/bin/osascript".into());
                let mut out = Vec::new();
                for line in macos_terminal_script(dir, program, args) {
                    out.push("-e".into());
                    out.push(line.into());
                }
                Some(OpenCommand::new(osascript, out, kind, false))
            }
        },
        Os::Windows => {
            let args: Vec<OsString> = args.iter().map(|a| one_line(a).into()).collect();
            let wt = match configured {
                Some(t) if t.eq_ignore_ascii_case("cmd") || t.eq_ignore_ascii_case("cmd.exe") => None,
                Some(t) => Some(find(t)?),
                None => find("wt"),
            };
            match wt {
                Some(wt) => {
                    let mut out: Vec<OsString> = vec!["-d".into(), dir.as_os_str().to_owned()];
                    // wt cannot start a batch shim directly.
                    if program
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
                    {
                        out.extend(["cmd.exe".into(), "/c".into()]);
                    }
                    out.push(program.as_os_str().to_owned());
                    // wt splits its command line into tabs at `;`.
                    out.extend(
                        args.iter()
                            .map(|a| a.to_string_lossy().replace(';', "\\;").into()),
                    );
                    Some(OpenCommand::new(wt, out, kind, false))
                }
                None => Some(OpenCommand {
                    new_console: true,
                    ..OpenCommand::new(program.to_path_buf(), args, kind, false)
                }),
            }
        }
    }
}

/// Lines of the AppleScript that opens a Terminal.app window in `dir` running `program args…`
/// (passed to `osascript` as `-e` arguments). The shell command uses `$'…'` quoting (zsh, bash),
/// so newlines and quotes in the prompt survive; it is then escaped as an AppleScript string.
pub fn macos_terminal_script(dir: &Path, program: &Path, args: &[OsString]) -> Vec<String> {
    let mut shell = format!(
        "cd {} && {}",
        ansi_c_quote(&dir.to_string_lossy()),
        ansi_c_quote(&program.to_string_lossy())
    );
    for a in args {
        shell.push(' ');
        shell.push_str(&ansi_c_quote(&a.to_string_lossy()));
    }
    let applescript = shell.replace('\\', "\\\\").replace('"', "\\\"");
    vec![
        "tell application \"Terminal\"".into(),
        "activate".into(),
        format!("do script \"{applescript}\""),
        "end tell".into(),
    ]
}

/// `$'…'` shell quoting: backslash, single quote and control characters are escaped.
fn ansi_c_quote(s: &str) -> String {
    let mut out = String::from("$'");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// Non-empty lines joined with ` / `.
fn one_line(s: &std::ffi::OsStr) -> String {
    s.to_string_lossy()
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" / ")
}

/// Like [`spawn_detached`], with the child's working directory set to `dir`. With
/// [`OpenCommand::new_console`] (Windows) the child gets its own console and keeps its stdio.
pub fn spawn_detached_in(cmd: &OpenCommand, dir: &Path) -> std::io::Result<()> {
    let mut command = std::process::Command::new(&cmd.program);
    command.args(&cmd.args).current_dir(dir);
    if cmd.new_console {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
            command.creation_flags(CREATE_NEW_CONSOLE);
        }
    } else {
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
    }
    let mut child = command.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Start `cmd` without waiting for it (OPEN-001): no stdio, and a background thread reaps the child
/// when it exits so no zombie is left behind. Errors only when the program cannot be started.
pub fn spawn_detached(cmd: &OpenCommand) -> std::io::Result<()> {
    let mut child = std::process::Command::new(&cmd.program)
        .args(&cmd.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

pub fn open_command(project: &Path, config: &OpenConfig) -> Option<OpenCommand> {
    open_command_for(
        &Platform::current(),
        project,
        config,
        &|name| find_program(name, std::env::var_os("PATH").as_deref()),
        &|p| p.exists(),
    )
}

/// [`open_command_for`] on Linux, where editors are looked up on `PATH` only.
pub fn open_command_with(
    project: &Path,
    config: &OpenConfig,
    find: &dyn Fn(&str) -> Option<PathBuf>,
) -> Option<OpenCommand> {
    open_command_for(
        &Platform::new(Os::Linux, |_| None),
        project,
        config,
        find,
        &|_| false,
    )
}

/// The editor `name` on `PATH`, then (XP-002) Windows `<name>64` (JetBrains' standalone
/// `bin` folder), Toolbox scripts, and macOS app bundles opened with `open -a`.
fn editor_command(
    platform: &Platform,
    name: &str,
    project: &Path,
    find: &dyn Fn(&str) -> Option<PathBuf>,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<(PathBuf, Vec<OsString>)> {
    let project_arg = || vec![project.as_os_str().to_owned()];
    if let Some(program) = find(name) {
        return Some((program, project_arg()));
    }
    if platform.os == Os::Windows
        && let Some(program) = find(&format!("{name}64"))
    {
        return Some((program, project_arg()));
    }
    if let Some(script) = platform.editor_scripts(name).into_iter().find(|p| exists(p)) {
        return Some((script, project_arg()));
    }
    let bundle = platform.app_bundles(name).into_iter().find(|p| exists(p))?;
    let open = find("open").unwrap_or_else(|| "/usr/bin/open".into());
    Some((
        open,
        vec![
            "-a".into(),
            bundle.into_os_string(),
            project.as_os_str().to_owned(),
        ],
    ))
}

pub fn open_command_for(
    platform: &Platform,
    project: &Path,
    config: &OpenConfig,
    find: &dyn Fn(&str) -> Option<PathBuf>,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<OpenCommand> {
    let kind = detect_kind(project);
    let editor = match kind {
        ProjectKind::Python => config.python.as_deref(),
        ProjectKind::Code => config.code.as_deref(),
        ProjectKind::Other => None,
    }
    .filter(|e| !e.is_empty());
    if let Some((program, args)) = editor.and_then(|e| editor_command(platform, e, project, find, exists)) {
        return Some(OpenCommand::new(program, args, kind, false));
    }
    let program = find(&config.directory)?;
    let args = vec![project.as_os_str().to_owned()];
    Some(OpenCommand::new(program, args, kind, editor.is_some()))
}

/// Resolve a program name on `path_var`; names containing a path separator are used as-is if they exist.
/// Look `name` up on `path_var` (or use it as a path), with `PATHEXT` suffixes on Windows (XP-001).
pub fn find_program(name: &str, path_var: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let path_var = path_var.map(|p| p.to_owned());
    let platform = crate::platform::Platform::new(crate::platform::Os::current(), move |k| {
        if k == "PATH" {
            path_var.clone()
        } else {
            std::env::var_os(k)
        }
    });
    platform.find_program(name, &is_executable)
}

pub(crate) fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn project(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for f in files {
            let p = dir.path().join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "").unwrap();
        }
        dir
    }

    #[test]
    fn kind_detection() {
        assert_eq!(
            detect_kind(project(&["pyproject.toml"]).path()),
            ProjectKind::Python
        );
        assert_eq!(
            detect_kind(project(&["requirements.txt", "package.json"]).path()),
            ProjectKind::Python
        );
        assert_eq!(detect_kind(project(&["package.json"]).path()), ProjectKind::Code);
        assert_eq!(detect_kind(project(&["Cargo.toml"]).path()), ProjectKind::Code);
        assert_eq!(
            detect_kind(project(&["main.py", "util.py", "x.sh"]).path()),
            ProjectKind::Python
        );
        assert_eq!(
            detect_kind(project(&["src/app.ts", "src/b.ts", "tool.py"]).path()),
            ProjectKind::Code
        );
        assert_eq!(
            detect_kind(project(&["notes.md", "photo.jpg"]).path()),
            ProjectKind::Other
        );
    }

    fn fake_find(available: &'static [&'static str]) -> impl Fn(&str) -> Option<PathBuf> {
        move |name| {
            available
                .contains(&name)
                .then(|| PathBuf::from("/usr/bin").join(name))
        }
    }

    #[test]
    fn command_per_kind_and_fallback() {
        let config = OpenConfig::default();
        let all = fake_find(&["pycharm", "webstorm", "xdg-open"]);
        let py = project(&["setup.py"]);
        let cmd = open_command_with(py.path(), &config, &all).unwrap();
        assert_eq!(
            (cmd.program, cmd.kind, cmd.fell_back),
            ("/usr/bin/pycharm".into(), ProjectKind::Python, false)
        );
        assert_eq!(cmd.args, [py.path().as_os_str()]);

        let js = project(&["package.json"]);
        assert_eq!(
            open_command_with(js.path(), &config, &all).unwrap().program,
            PathBuf::from("/usr/bin/webstorm")
        );

        let other = project(&["notes.md"]);
        let cmd = open_command_with(other.path(), &config, &all).unwrap();
        assert_eq!((cmd.program, cmd.fell_back), ("/usr/bin/xdg-open".into(), false));

        let no_editors = fake_find(&["xdg-open"]);
        let cmd = open_command_with(js.path(), &config, &no_editors).unwrap();
        assert_eq!((cmd.program, cmd.fell_back), ("/usr/bin/xdg-open".into(), true));
        assert!(open_command_with(js.path(), &config, &fake_find(&[])).is_none());

        let disabled = OpenConfig {
            code: Some(String::new()),
            ..OpenConfig::default()
        };
        let cmd = open_command_with(js.path(), &disabled, &all).unwrap();
        assert_eq!((cmd.program, cmd.fell_back), ("/usr/bin/xdg-open".into(), false));
    }

    #[test]
    fn terminal_arguments_per_terminal() {
        let dir = Path::new("/home/u/My Project");
        let prog = Path::new("/usr/bin/claude");
        let args = [OsString::from("Resume: fix the tests")];
        let cmd = |t: &str| {
            terminal_command(Path::new(t), dir, prog, &args)
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            cmd("/usr/bin/gnome-terminal"),
            [
                "--working-directory=/home/u/My Project",
                "--",
                "/usr/bin/claude",
                "Resume: fix the tests"
            ]
        );
        assert_eq!(cmd("/usr/bin/gnome-terminal.wrapper")[1], "--");
        assert_eq!(
            cmd("konsole"),
            [
                "--workdir",
                "/home/u/My Project",
                "-e",
                "/usr/bin/claude",
                "Resume: fix the tests"
            ]
        );
        assert_eq!(cmd("/usr/bin/kitty")[0], "--directory=/home/u/My Project");
        assert_eq!(
            &cmd("wezterm")[..4],
            ["start", "--cwd", "/home/u/My Project", "--"]
        );
        assert_eq!(
            cmd("/opt/odd-term"),
            ["-e", "/usr/bin/claude", "Resume: fix the tests"]
        );
    }

    #[test]
    fn terminal_choice() {
        let find = fake_find(&["konsole", "xterm"]);
        assert_eq!(
            find_terminal(None, &find),
            Some(PathBuf::from("/usr/bin/konsole"))
        );
        assert_eq!(
            find_terminal(Some("xterm"), &find),
            Some(PathBuf::from("/usr/bin/xterm"))
        );
        assert_eq!(
            find_terminal(Some("kitty"), &find),
            None,
            "a configured terminal is not replaced"
        );
        assert_eq!(
            find_terminal(Some(""), &find),
            Some(PathBuf::from("/usr/bin/konsole"))
        );
        assert_eq!(find_terminal(None, &fake_find(&[])), None);
    }

    fn on(os: Os, vars: &'static [(&'static str, &'static str)]) -> Platform<'static> {
        Platform::new(os, move |k| {
            vars.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v))
        })
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn macos_editors_use_path_toolbox_then_app_bundles() {
        let mac = on(Os::MacOs, &[("HOME", "/Users/u")]);
        let config = OpenConfig::default();
        let js = project(&["package.json"]);
        let only_open = |n: &str| (n == "open").then(|| PathBuf::from("/usr/bin/open"));
        let bundle = |p: &Path| p == Path::new("/Users/u/Applications/WebStorm.app");
        let cmd = open_command_for(&mac, js.path(), &config, &only_open, &bundle).unwrap();
        assert_eq!(cmd.program, PathBuf::from("/usr/bin/open"));
        assert_eq!(
            strings(&cmd.args),
            [
                "-a",
                "/Users/u/Applications/WebStorm.app",
                &js.path().to_string_lossy()
            ]
        );
        assert_eq!((cmd.label(), cmd.fell_back), ("WebStorm".into(), false));

        let toolbox = |p: &Path| p.ends_with("Toolbox/scripts/webstorm");
        let cmd = open_command_for(&mac, js.path(), &config, &only_open, &toolbox).unwrap();
        assert_eq!(
            cmd.program,
            PathBuf::from("/Users/u/Library/Application Support/JetBrains/Toolbox/scripts/webstorm")
        );
        let on_path = fake_find(&["webstorm", "open"]);
        let cmd = open_command_for(&mac, js.path(), &config, &on_path, &bundle).unwrap();
        assert_eq!(cmd.label(), "webstorm");

        let mac_config = OpenConfig {
            directory: "open".into(),
            ..OpenConfig::default()
        };
        let cmd = open_command_for(&mac, js.path(), &mac_config, &only_open, &|_| false).unwrap();
        assert_eq!((cmd.label(), cmd.fell_back), ("open".into(), true));
        assert_eq!(strings(&cmd.args), [js.path().to_string_lossy()]);
    }

    #[test]
    fn windows_editors_try_64_suffix_and_toolbox_scripts() {
        let win = on(
            Os::Windows,
            &[
                ("USERPROFILE", r"C:\Users\u"),
                ("LOCALAPPDATA", r"C:\Users\u\AppData\Local"),
            ],
        );
        let config = OpenConfig {
            directory: "explorer".into(),
            ..OpenConfig::default()
        };
        let py = project(&["setup.py"]);
        let find = |n: &str| match n {
            "pycharm64" => Some(PathBuf::from(r"C:\JetBrains\PyCharm\bin\pycharm64.exe")),
            "explorer" => Some(PathBuf::from(r"C:\Windows\explorer.exe")),
            _ => None,
        };
        let cmd = open_command_for(&win, py.path(), &config, &find, &|_| false).unwrap();
        assert_eq!((cmd.label(), cmd.fell_back), ("pycharm64".into(), false));

        let js = project(&["package.json"]);
        let script = |p: &Path| p.to_string_lossy().ends_with("webstorm.cmd");
        let cmd = open_command_for(&win, js.path(), &config, &find, &script).unwrap();
        assert!(cmd.program.to_string_lossy().ends_with("webstorm.cmd"));
        assert_eq!(cmd.label(), "webstorm");

        let cmd = open_command_for(&win, js.path(), &config, &find, &|_| false).unwrap();
        assert_eq!((cmd.label(), cmd.fell_back), ("explorer".into(), true));
        assert!(win.app_bundles("webstorm").is_empty(), "no `open -a` on Windows");
    }

    #[test]
    fn macos_agent_terminal() {
        let mac = on(Os::MacOs, &[("HOME", "/Users/u")]);
        let dir = Path::new("/Users/u/My 'Proj'");
        let prog = Path::new("/opt/homebrew/bin/claude");
        let args = [OsString::from("Context: \"x\"\n1. fix \\tests")];
        let find = fake_find(&["osascript", "kitty"]);
        let cmd = agent_terminal(&mac, None, &find, dir, prog, &args).unwrap();
        assert_eq!(cmd.program, PathBuf::from("/usr/bin/osascript"));
        assert_eq!(cmd.terminal_label(), "Terminal");
        assert_eq!(
            strings(&cmd.args),
            [
                "-e",
                "tell application \"Terminal\"",
                "-e",
                "activate",
                "-e",
                r#"do script "cd $'/Users/u/My \\'Proj\\'' && $'/opt/homebrew/bin/claude' $'Context: \"x\"\\n1. fix \\\\tests'""#,
                "-e",
                "end tell"
            ]
        );
        assert_eq!(
            agent_terminal(&mac, Some("Terminal"), &find, dir, prog, &args)
                .unwrap()
                .program,
            PathBuf::from("/usr/bin/osascript")
        );
        let kitty = agent_terminal(&mac, Some("kitty"), &find, dir, prog, &args).unwrap();
        assert_eq!(kitty.program, PathBuf::from("/usr/bin/kitty"));
        assert_eq!(strings(&kitty.args)[0], "--directory=/Users/u/My 'Proj'");
        assert!(agent_terminal(&mac, Some("iterm-missing"), &find, dir, prog, &args).is_none());
    }

    #[test]
    fn windows_agent_terminal() {
        let win = on(Os::Windows, &[("USERPROFILE", r"C:\Users\u")]);
        let dir = Path::new(r"C:\Users\u\My Project");
        let prog = Path::new(r"C:\Users\u\AppData\Roaming\npm\codex.cmd");
        let args = [OsString::from("About: a; b\n\n1. fix tests\r\n2. ship")];
        let wt = |n: &str| {
            (n == "wt").then(|| PathBuf::from(r"C:\Users\u\AppData\Local\Microsoft\WindowsApps\wt.exe"))
        };
        let cmd = agent_terminal(&win, None, &wt, dir, prog, &args).unwrap();
        assert!(!cmd.new_console);
        assert_eq!(cmd.terminal_label(), "wt");
        assert_eq!(
            strings(&cmd.args),
            [
                "-d",
                r"C:\Users\u\My Project",
                "cmd.exe",
                "/c",
                r"C:\Users\u\AppData\Roaming\npm\codex.cmd",
                r"About: a\; b / 1. fix tests / 2. ship"
            ]
        );
        let exe = Path::new(r"C:\Users\u\.local\bin\claude.exe");
        let cmd = agent_terminal(&win, None, &wt, dir, exe, &args).unwrap();
        assert_eq!(strings(&cmd.args)[2], r"C:\Users\u\.local\bin\claude.exe");

        // No Windows Terminal, or `cmd` configured: the agent in its own console window.
        for (configured, find) in [
            (None, &fake_find(&[]) as &dyn Fn(&str) -> Option<PathBuf>),
            (Some("cmd"), &wt),
        ] {
            let cmd = agent_terminal(&win, configured, find, dir, prog, &args).unwrap();
            assert!(cmd.new_console);
            assert_eq!(cmd.terminal_label(), "a new console window");
            assert_eq!(cmd.program, prog);
            assert_eq!(strings(&cmd.args), ["About: a; b / 1. fix tests / 2. ship"]);
        }
        assert!(agent_terminal(&win, Some("alacritty"), &wt, dir, prog, &args).is_none());
    }

    #[test]
    fn linux_agent_terminal_is_unchanged() {
        let linux = on(Os::Linux, &[("HOME", "/home/u")]);
        let find = fake_find(&["konsole"]);
        let args = [OsString::from("hi\nthere")];
        let cmd = agent_terminal(
            &linux,
            None,
            &find,
            Path::new("/p"),
            Path::new("/usr/bin/codex"),
            &args,
        )
        .unwrap();
        assert_eq!(cmd.program, PathBuf::from("/usr/bin/konsole"));
        assert_eq!(
            strings(&cmd.args),
            ["--workdir", "/p", "-e", "/usr/bin/codex", "hi\nthere"]
        );
        assert!(
            agent_terminal(
                &linux,
                None,
                &fake_find(&[]),
                Path::new("/p"),
                Path::new("/x"),
                &args
            )
            .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn spawn_detached_runs_without_waiting() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("editor");
        let marker = dir.path().join("opened");
        fs::write(
            &exe,
            "#!/bin/sh\nsleep 0.2\necho \"$1\" > \"$(dirname \"$0\")/opened\"\n",
        )
        .unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        let cmd = OpenCommand {
            program: exe,
            args: vec!["/some/project".into()],
            kind: ProjectKind::Code,
            fell_back: false,
            new_console: false,
        };
        let start = std::time::Instant::now();
        spawn_detached(&cmd).unwrap();
        assert!(
            start.elapsed() < std::time::Duration::from_millis(150),
            "returns before the program ends"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !marker.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(fs::read_to_string(&marker).unwrap().trim(), "/some/project");
        let missing = OpenCommand {
            program: "/nonexistent/editor".into(),
            ..cmd
        };
        assert!(spawn_detached(&missing).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn path_lookup() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("pycharm");
        fs::write(&exe, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(dir.path().join("noexec"), "").unwrap();
        let path = std::env::join_paths([Path::new("/nonexistent"), dir.path()]).unwrap();
        assert_eq!(find_program("pycharm", Some(&path)), Some(exe.clone()));
        assert_eq!(find_program("noexec", Some(&path)), None);
        assert_eq!(find_program(exe.to_str().unwrap(), None), Some(exe));
    }
}
