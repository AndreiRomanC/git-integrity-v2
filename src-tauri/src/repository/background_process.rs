//! Child processes whose output belongs in the app, not a separate console.
use std::{ffi::OsStr, process::Command};

pub(super) fn command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW affects console allocation only. Keep pipes, exit
        // codes, credentials, timeouts and GUI tools/browser sign-in unchanged.
        command.creation_flags(0x08000000);
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_command_preserves_program_arguments_environment_and_directory() {
        let mut child = command("git");
        let directory = std::env::temp_dir();
        child.args(["status", "--short"]).current_dir(&directory).env("GDD_PROCESS_TEST", "value");
        assert_eq!(child.get_program(), "git");
        assert_eq!(child.get_args().collect::<Vec<_>>(), vec!["status", "--short"]);
        assert_eq!(child.get_current_dir(), Some(directory.as_path()));
        assert!(child.get_envs().any(|(key, value)| key == "GDD_PROCESS_TEST" && value == Some(OsStr::new("value"))));
    }

    #[test]
    fn background_command_preserves_output_and_failure_status() {
        #[cfg(windows)]
        let mut child = {
            let mut child = command("cmd.exe");
            child.args(["/D", "/C", "echo output& echo diagnostic 1>&2& exit /B 7"]);
            child
        };
        #[cfg(not(windows))]
        let mut child = {
            let mut child = command("/bin/sh");
            child.args(["-c", "printf output; printf diagnostic >&2; exit 7"]);
            child
        };
        let output = child.output().unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "output");
        assert_eq!(String::from_utf8_lossy(&output.stderr).trim(), "diagnostic");
    }

    #[cfg(windows)]
    #[test]
    fn background_command_has_no_windows_console() {
        // Runs only on Windows: ask the child itself whether it owns a console.
        // PowerShell is used only by this regression test, never by the app.
        let output = command("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command",
            "Add-Type -Name ConsoleProbe -Namespace GddTest -MemberDefinition '[System.Runtime.InteropServices.DllImport(\"kernel32.dll\")] public static extern System.IntPtr GetConsoleWindow();'; if ([GddTest.ConsoleProbe]::GetConsoleWindow() -ne [IntPtr]::Zero) { exit 1 }; Write-Output 'no-console'"
        ]).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "no-console");
    }
}
