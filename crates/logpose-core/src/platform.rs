//! Per-OS locations (XP-001): home, config and data directories, agent log bases, and program
//! lookup on `PATH` (with `PATHEXT` on Windows). Everything resolves through [`Platform`], which
//! takes the OS and the environment as inputs, so every OS is unit-tested on any machine.
//!
//! Conventions match the `dirs` crate: Linux uses the XDG variables, macOS
//! `~/Library/Application Support` for config and data, Windows `%APPDATA%` (roaming).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    MacOs,
    Windows,
}

impl Os {
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Linux
        }
    }
}

/// Reads an environment variable.
type EnvFn<'a> = Box<dyn Fn(&str) -> Option<OsString> + 'a>;

/// An OS plus a way to read environment variables.
pub struct Platform<'a> {
    pub os: Os,
    env: EnvFn<'a>,
}

impl Platform<'static> {
    /// This machine.
    pub fn current() -> Self {
        Platform {
            os: Os::current(),
            env: Box::new(|k| std::env::var_os(k)),
        }
    }
}

impl<'a> Platform<'a> {
    pub fn new(os: Os, env: impl Fn(&str) -> Option<OsString> + 'a) -> Self {
        Self {
            os,
            env: Box::new(env),
        }
    }

    /// A non-empty environment variable.
    fn var(&self, key: &str) -> Option<OsString> {
        (self.env)(key).filter(|v| !v.is_empty())
    }

    fn var_path(&self, key: &str) -> Option<PathBuf> {
        self.var(key).map(PathBuf::from)
    }

    /// `$HOME` on Unix; `%USERPROFILE%`, then `%HOMEDRIVE%%HOMEPATH%`, on Windows.
    pub fn home(&self) -> Option<PathBuf> {
        match self.os {
            Os::Windows => self.var_path("USERPROFILE").or_else(|| {
                let mut drive = self.var("HOMEDRIVE")?;
                drive.push(self.var("HOMEPATH")?);
                Some(PathBuf::from(drive))
            }),
            _ => self.var_path("HOME"),
        }
    }

    /// Per-user config root: `$XDG_CONFIG_HOME` or `~/.config`; `~/Library/Application Support`;
    /// `%APPDATA%` (or `~\AppData\Roaming`).
    pub fn config_dir(&self) -> Option<PathBuf> {
        match self.os {
            Os::Linux => self
                .var_path("XDG_CONFIG_HOME")
                .filter(|p| p.is_absolute())
                .or_else(|| Some(self.home()?.join(".config"))),
            Os::MacOs => Some(self.home()?.join("Library").join("Application Support")),
            Os::Windows => self.appdata(),
        }
    }

    /// Per-user data root: `$XDG_DATA_HOME` or `~/.local/share`; `~/Library/Application Support`;
    /// `%APPDATA%`.
    pub fn data_dir(&self) -> Option<PathBuf> {
        match self.os {
            Os::Linux => self
                .var_path("XDG_DATA_HOME")
                .filter(|p| p.is_absolute())
                .or_else(|| Some(self.home()?.join(".local").join("share"))),
            Os::MacOs => Some(self.home()?.join("Library").join("Application Support")),
            Os::Windows => self.appdata(),
        }
    }

    fn appdata(&self) -> Option<PathBuf> {
        self.var_path("APPDATA")
            .or_else(|| Some(self.home()?.join("AppData").join("Roaming")))
    }

    /// `$env_var` if set, else `~/<default>`; agents use the same layout on every OS
    /// (`CLAUDE_CONFIG_DIR` → `~/.claude`, `CODEX_HOME` → `~/.codex`).
    pub fn home_subdir(&self, env_var: &str, default: &str) -> Option<PathBuf> {
        self.var_path(env_var)
            .or_else(|| Some(self.home()?.join(default)))
    }

    /// Separator of `PATH` entries for this OS.
    pub(crate) fn path_separator(&self) -> char {
        if self.os == Os::Windows { ';' } else { ':' }
    }

    /// Executable suffixes tried on Windows (`%PATHEXT%`, default `.COM;.EXE;.BAT;.CMD`);
    /// empty elsewhere.
    fn exe_suffixes(&self) -> Vec<String> {
        if self.os != Os::Windows {
            return vec![String::new()];
        }
        let ext = self
            .var("PATHEXT")
            .map(|v| v.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
        let mut out = vec![String::new()];
        out.extend(ext.split(';').filter(|e| !e.is_empty()).map(|e| e.to_lowercase()));
        out
    }

    /// Candidate paths for `name`: as given when it has a directory part, else each `PATH` entry,
    /// with Windows executable suffixes when `name` has no extension. `is_exe` decides.
    pub fn find_program(&self, name: &str, is_exe: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
        let has_dir = name.contains('/') || (self.os == Os::Windows && name.contains('\\'));
        let has_ext = Path::new(name).extension().is_some();
        let suffixes = if has_ext {
            vec![String::new()]
        } else {
            self.exe_suffixes()
        };
        let with_suffixes = |base: PathBuf| {
            suffixes.iter().find_map(|s| {
                let mut p = base.clone().into_os_string();
                p.push(s);
                let p = PathBuf::from(p);
                is_exe(&p).then_some(p)
            })
        };
        if has_dir {
            return with_suffixes(PathBuf::from(name));
        }
        let path = self.var("PATH")?;
        path.to_string_lossy()
            .split(self.path_separator())
            .filter(|d| !d.is_empty())
            .find_map(|dir| with_suffixes(Path::new(dir).join(name)))
    }

    /// Install folders searched after `PATH` (AGENT-001): an app started from the desktop menu gets
    /// the session's short `PATH`, which often lacks the per-user folders where `claude` / `codex`
    /// (native installer, npm, bun, volta) live. The caller keeps the ones that exist.
    pub fn extra_bin_dirs(&self) -> Vec<PathBuf> {
        let home = self.home();
        let under = |rel: &[&str]| {
            home.as_ref()
                .map(|h| rel.iter().fold(h.clone(), |p, r| p.join(r)))
        };
        let mut dirs: Vec<Option<PathBuf>> = match self.os {
            Os::Windows => vec![
                under(&[".local", "bin"]),
                self.appdata().map(|a| a.join("npm")),
                under(&[".bun", "bin"]),
                under(&[".volta", "bin"]),
            ],
            _ => vec![
                under(&[".local", "bin"]),
                under(&[".claude", "local"]),
                under(&[".npm-global", "bin"]),
                under(&[".bun", "bin"]),
                under(&[".volta", "bin"]),
                under(&[".yarn", "bin"]),
                under(&["bin"]),
                Some("/usr/local/bin".into()),
            ],
        };
        match self.os {
            Os::Linux => dirs.push(Some("/snap/bin".into())),
            Os::MacOs => dirs.push(Some("/opt/homebrew/bin".into())),
            Os::Windows => {}
        }
        dirs.into_iter().flatten().collect()
    }

    /// nvm's installed Node versions (`$NVM_DIR` or `~/.nvm`, then `versions/node`); not on Windows,
    /// where nvm-windows puts the active Node on `PATH` itself.
    pub fn nvm_versions_dir(&self) -> Option<PathBuf> {
        if self.os == Os::Windows {
            return None;
        }
        let root = self
            .var_path("NVM_DIR")
            .or_else(|| Some(self.home()?.join(".nvm")))?;
        Some(root.join("versions").join("node"))
    }

    /// Where the Codex CLI may be installed besides `PATH` (only the Linux path is verified).
    pub fn codex_fallbacks(&self) -> Vec<PathBuf> {
        match self.os {
            Os::Linux => vec!["/usr/lib/chatgpt/resources/codex".into()],
            Os::MacOs => vec!["/Applications/ChatGPT.app/Contents/Resources/codex".into()],
            // npm's global shim (`npm i -g @openai/codex`).
            Os::Windows => self
                .appdata()
                .map(|a| a.join("npm").join("codex.cmd"))
                .into_iter()
                .collect(),
        }
    }

    /// macOS app bundles that may hold the editor `name` (XP-002): `/Applications` and
    /// `~/Applications` (JetBrains Toolbox), for known bundle names of `pycharm` / `webstorm`, else
    /// `name` itself (so `code = "Visual Studio Code"` works). Empty on other OSes.
    pub fn app_bundles(&self, name: &str) -> Vec<PathBuf> {
        if self.os != Os::MacOs {
            return Vec::new();
        }
        let names: &[&str] = match name {
            "pycharm" => &[
                "PyCharm",
                "PyCharm CE",
                "PyCharm Professional Edition",
                "PyCharm Community Edition",
            ],
            "webstorm" => &["WebStorm"],
            other => &[other][..],
        };
        let mut dirs = vec![PathBuf::from("/Applications")];
        dirs.extend(self.home().map(|h| h.join("Applications")));
        names
            .iter()
            .flat_map(|n| {
                let n = n.trim_end_matches(".app");
                dirs.iter().map(move |d| d.join(format!("{n}.app")))
            })
            .collect()
    }

    /// Launcher scripts for the editor `name` besides `PATH` (XP-002): JetBrains Toolbox scripts on
    /// macOS (`~/Library/Application Support/JetBrains/Toolbox/scripts/<name>`) and Windows
    /// (`%LOCALAPPDATA%\JetBrains\Toolbox\scripts\<name>.cmd`). Empty on Linux, where they are on `PATH`.
    pub fn editor_scripts(&self, name: &str) -> Vec<PathBuf> {
        let scripts = |base: PathBuf| base.join("JetBrains").join("Toolbox").join("scripts");
        match self.os {
            Os::Linux => Vec::new(),
            Os::MacOs => self
                .data_dir()
                .map(|d| scripts(d).join(name))
                .into_iter()
                .collect(),
            Os::Windows => self
                .var_path("LOCALAPPDATA")
                .or_else(|| Some(self.home()?.join("AppData").join("Local")))
                .map(|d| scripts(d).join(format!("{name}.cmd")))
                .into_iter()
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn platform(os: Os, vars: &[(&str, &str)]) -> Platform<'static> {
        let map: HashMap<String, OsString> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        Platform::new(os, move |k| map.get(k).cloned())
    }

    #[test]
    fn linux_dirs_follow_xdg() {
        let p = platform(Os::Linux, &[("HOME", "/home/u")]);
        assert_eq!(p.home(), Some("/home/u".into()));
        assert_eq!(p.config_dir(), Some("/home/u/.config".into()));
        assert_eq!(p.data_dir(), Some("/home/u/.local/share".into()));
        let p = platform(
            Os::Linux,
            &[
                ("HOME", "/home/u"),
                ("XDG_CONFIG_HOME", "/tmp/cfg"),
                ("XDG_DATA_HOME", "relative/ignored"),
            ],
        );
        assert_eq!(p.config_dir(), Some("/tmp/cfg".into()));
        assert_eq!(
            p.data_dir(),
            Some("/home/u/.local/share".into()),
            "relative XDG paths are invalid"
        );
        assert_eq!(platform(Os::Linux, &[]).config_dir(), None);
    }

    #[test]
    fn macos_dirs_use_application_support() {
        let p = platform(
            Os::MacOs,
            &[("HOME", "/Users/u"), ("XDG_CONFIG_HOME", "/ignored")],
        );
        let support = Path::new("/Users/u").join("Library").join("Application Support");
        assert_eq!(p.config_dir(), Some(support.clone()));
        assert_eq!(p.data_dir(), Some(support));
    }

    #[test]
    fn windows_dirs_use_userprofile_and_appdata() {
        let p = platform(
            Os::Windows,
            &[
                ("USERPROFILE", r"C:\Users\u"),
                ("APPDATA", r"C:\Users\u\AppData\Roaming"),
                ("HOME", "/ignored"),
            ],
        );
        assert_eq!(p.home(), Some(r"C:\Users\u".into()));
        assert_eq!(p.config_dir(), Some(r"C:\Users\u\AppData\Roaming".into()));
        assert_eq!(p.data_dir(), p.config_dir());
        // Without USERPROFILE: HOMEDRIVE + HOMEPATH; without APPDATA: home\AppData\Roaming.
        let p = platform(Os::Windows, &[("HOMEDRIVE", "D:"), ("HOMEPATH", r"\Users\v")]);
        assert_eq!(p.home(), Some(r"D:\Users\v".into()));
        assert_eq!(
            p.config_dir(),
            Some(Path::new(r"D:\Users\v").join("AppData").join("Roaming"))
        );
    }

    #[test]
    fn agent_bases_honor_overrides_on_every_os() {
        for (os, home_var, home) in [
            (Os::Linux, "HOME", "/home/u"),
            (Os::MacOs, "HOME", "/Users/u"),
            (Os::Windows, "USERPROFILE", r"C:\Users\u"),
        ] {
            let p = platform(os, &[(home_var, home)]);
            assert_eq!(
                p.home_subdir("CLAUDE_CONFIG_DIR", ".claude"),
                Some(Path::new(home).join(".claude"))
            );
            assert_eq!(
                p.home_subdir("CODEX_HOME", ".codex"),
                Some(Path::new(home).join(".codex"))
            );
            let p = platform(
                os,
                &[
                    (home_var, home),
                    ("CLAUDE_CONFIG_DIR", "/x/claude"),
                    ("CODEX_HOME", ""),
                ],
            );
            assert_eq!(
                p.home_subdir("CLAUDE_CONFIG_DIR", ".claude"),
                Some("/x/claude".into())
            );
            assert_eq!(
                p.home_subdir("CODEX_HOME", ".codex"),
                Some(Path::new(home).join(".codex")),
                "empty = unset"
            );
        }
    }

    #[test]
    fn program_lookup_per_os() {
        let exists =
            |set: &'static [&'static str]| move |p: &Path| set.contains(&p.to_string_lossy().as_ref());
        // Unix: ':'-separated PATH, no suffixes.
        let p = platform(Os::Linux, &[("PATH", "/usr/local/bin:/usr/bin")]);
        assert_eq!(
            p.find_program("codex", &exists(&["/usr/bin/codex"])),
            Some("/usr/bin/codex".into())
        );
        assert_eq!(
            p.find_program("/opt/codex", &exists(&["/opt/codex"])),
            Some("/opt/codex".into())
        );
        assert_eq!(p.find_program("codex", &exists(&[])), None);
        // Windows: ';'-separated PATH, PATHEXT suffixes (lowercased), first directory wins.
        let p = platform(
            Os::Windows,
            &[("PATH", r"C:\bin;C:\npm"), ("PATHEXT", ".EXE;.CMD")],
        );
        let found = p.find_program("codex", &|q: &Path| {
            q.to_string_lossy().ends_with("codex.cmd") && q.starts_with(r"C:\npm")
        });
        assert_eq!(found, Some(Path::new(r"C:\npm").join("codex.cmd")));
        assert_eq!(
            p.find_program("codex.exe", &|q: &Path| q.ends_with("codex.exe")),
            Some(Path::new(r"C:\bin").join("codex.exe"))
        );
        // A Windows path with backslashes counts as a path, not a name.
        assert_eq!(
            p.find_program(r"D:\tools\codex", &|q: &Path| q.to_string_lossy()
                == r"D:\tools\codex.exe"),
            Some(r"D:\tools\codex.exe".into())
        );
        // Default PATHEXT.
        let p = platform(Os::Windows, &[("PATH", r"C:\bin")]);
        assert!(
            p.find_program("pycharm", &|q: &Path| q.to_string_lossy().ends_with(".bat"))
                .is_some()
        );
    }

    #[test]
    fn extra_bin_dirs_per_os() {
        let linux = platform(Os::Linux, &[("HOME", "/home/u")]).extra_bin_dirs();
        assert_eq!(linux[0], PathBuf::from("/home/u/.local/bin"));
        assert!(linux.contains(&"/home/u/.claude/local".into()));
        assert!(linux.contains(&"/home/u/.npm-global/bin".into()));
        assert_eq!(linux.last(), Some(&PathBuf::from("/snap/bin")));
        let mac = platform(Os::MacOs, &[("HOME", "/Users/u")]).extra_bin_dirs();
        assert_eq!(mac.last(), Some(&PathBuf::from("/opt/homebrew/bin")));
        let win = platform(Os::Windows, &[("USERPROFILE", r"C:\U"), ("APPDATA", r"C:\R")]);
        let win = win.extra_bin_dirs();
        assert_eq!(win[0], Path::new(r"C:\U").join(".local").join("bin"));
        assert_eq!(win[1], Path::new(r"C:\R").join("npm"));
        // Without a home only the fixed folders remain.
        assert_eq!(
            platform(Os::Linux, &[]).extra_bin_dirs(),
            [PathBuf::from("/usr/local/bin"), "/snap/bin".into()]
        );
    }

    #[test]
    fn nvm_versions_dir_per_os() {
        let p = platform(Os::Linux, &[("HOME", "/home/u")]);
        assert_eq!(p.nvm_versions_dir(), Some("/home/u/.nvm/versions/node".into()));
        let p = platform(Os::MacOs, &[("HOME", "/Users/u"), ("NVM_DIR", "/opt/nvm")]);
        assert_eq!(p.nvm_versions_dir(), Some("/opt/nvm/versions/node".into()));
        assert_eq!(
            platform(Os::Windows, &[("USERPROFILE", r"C:\U")]).nvm_versions_dir(),
            None
        );
    }

    #[test]
    fn codex_fallbacks_per_os() {
        assert_eq!(
            platform(Os::Linux, &[]).codex_fallbacks(),
            [PathBuf::from("/usr/lib/chatgpt/resources/codex")]
        );
        assert_eq!(
            platform(Os::MacOs, &[]).codex_fallbacks(),
            [PathBuf::from(
                "/Applications/ChatGPT.app/Contents/Resources/codex"
            )]
        );
        assert_eq!(
            platform(Os::Windows, &[("APPDATA", r"C:\R")]).codex_fallbacks(),
            [Path::new(r"C:\R").join("npm").join("codex.cmd")]
        );
    }

    #[test]
    fn editor_fallbacks_per_os() {
        let mac = platform(Os::MacOs, &[("HOME", "/Users/u")]);
        assert_eq!(
            mac.app_bundles("webstorm"),
            [
                PathBuf::from("/Applications/WebStorm.app"),
                "/Users/u/Applications/WebStorm.app".into()
            ]
        );
        assert_eq!(mac.app_bundles("pycharm").len(), 8);
        assert_eq!(
            mac.app_bundles("pycharm")[2],
            PathBuf::from("/Applications/PyCharm CE.app")
        );
        assert_eq!(
            mac.app_bundles("Visual Studio Code.app")[0],
            PathBuf::from("/Applications/Visual Studio Code.app")
        );
        assert_eq!(
            mac.editor_scripts("pycharm"),
            [PathBuf::from(
                "/Users/u/Library/Application Support/JetBrains/Toolbox/scripts/pycharm"
            )]
        );
        let win = platform(
            Os::Windows,
            &[("USERPROFILE", r"C:\Users\u"), ("LOCALAPPDATA", r"D:\Local")],
        );
        assert!(win.app_bundles("pycharm").is_empty());
        assert_eq!(
            win.editor_scripts("webstorm"),
            [Path::new(r"D:\Local")
                .join("JetBrains")
                .join("Toolbox")
                .join("scripts")
                .join("webstorm.cmd")]
        );
        let win = platform(Os::Windows, &[("USERPROFILE", r"C:\Users\u")]);
        assert_eq!(
            win.editor_scripts("pycharm")[0],
            Path::new(r"C:\Users\u")
                .join("AppData")
                .join("Local")
                .join("JetBrains")
                .join("Toolbox")
                .join("scripts")
                .join("pycharm.cmd")
        );
        let linux = platform(Os::Linux, &[("HOME", "/home/u")]);
        assert!(linux.app_bundles("pycharm").is_empty() && linux.editor_scripts("pycharm").is_empty());
    }

    #[test]
    fn current_matches_this_machine() {
        let p = Platform::current();
        assert_eq!(p.os, Os::current());
        if cfg!(target_os = "linux") {
            assert_eq!(p.home(), std::env::var_os("HOME").map(PathBuf::from));
        }
    }
}
