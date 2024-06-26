//! Helper traits and types to make `std::process::Command` more ergonomic

#![warn(missing_docs)]

use std::process::{Command as osCommand, Output};
use tracing::{debug, error, info};

/// `Ok(Output)` when a child process successfully runs and returns exit code `0`.
///
/// All other circumstances are regarded as `Err(CommandError)`. This includes
/// when there is an error invoking a child process, if a child process is
/// terminated, or if the child process runs and returns a non-zero exit code.
pub type CommandResult = Result<Output, CommandError>;

/// An error type describing the kinds of failure a child process may have
#[derive(thiserror::Error, Debug)]
#[error("Failed to run \"{command}\"")]
pub struct CommandError {
    /// The command that was attempting to run
    pub command: String,
    /// The kind of error that the command encountered
    pub source: CommandErrorKind,
}

/// Describes the particular kinds of errors that may occur while running a command
#[derive(thiserror::Error, Debug)]
pub enum CommandErrorKind {
    /// The child process was terminated, so did not exit successfully
    #[error("Child process was terminated and has no exit code")]
    Terminated,
    /// The child process completed with a non-zero exit code
    #[error("\
Child process completed with non-zero exit code {0}
  stdout: {}
  stderr: {}",
        String::from_utf8_lossy(&.1.stdout).to_string(),
        String::from_utf8_lossy(&.1.stderr).to_string())]
    ExitError(i32, Output),
    /// There was an error invoking the command
    #[error("An error occurred while invoking child process")]
    IoError(#[from] std::io::Error),

    #[error("An error occurred while trying to connect")]
    ConnectivityError(CommandConnectivityError),
}

/// Adds useful extension methods to the `Command` type
pub trait CommandExt {
    /// Inherit both `stdout` and `stderr` from this process.
    ///
    /// # Example
    ///
    /// ```
    /// use std::process::Command;
    /// use fluvio_command::CommandExt;
    /// let _ = Command::new("echo")
    ///     .arg("Hello world")
    ///     .inherit()
    ///     .status();
    /// ```
    fn inherit(&mut self) -> &mut Self;
    /// Print a stringified version of the Command to the debug log.
    ///
    /// # Example
    ///
    /// ```
    /// use std::process::Command;
    /// use fluvio_command::CommandExt;
    /// let _ = Command::new("echo")
    ///     .arg("How are you Fluvio")
    ///     .log()
    ///     .spawn();
    /// ```
    fn log(&mut self) -> &mut Self;
    /// Print a stringified version of the Command to stdout.
    ///
    /// # Example
    ///
    /// ```
    /// use std::process::Command;
    /// use fluvio_command::CommandExt;
    /// let _ = Command::new("echo")
    ///     .arg("How are you Fluvio")
    ///     .print()
    ///     .spawn();
    /// ```
    fn print(&mut self) -> &mut Self;
    /// Return a stringified version of the Command.
    ///
    /// # Example
    ///
    /// ```
    /// use std::process::Command;
    /// use fluvio_command::CommandExt;
    /// let mut command = Command::new("echo");
    /// command.arg("one").arg("two three");
    /// let command_string: String = command.display();
    /// assert_eq!(command_string, "echo one two three");
    /// ```
    fn display(&self) -> String;
    /// Returns a result signaling the outcome of executing this command.
    ///
    /// # Example
    ///
    /// ```
    /// use std::process::{Command, Output};
    /// use fluvio_command::{CommandExt, CommandErrorKind};
    ///
    /// // On success, we get the stdout and stderr output
    /// let output: Output = Command::new("true").result().unwrap();
    ///
    /// let error = Command::new("bash")
    ///     .args(&["-c", r#"echo "this command failed with this stderr" 1>&2 && false"#])
    ///     .result()
    ///     .unwrap_err();
    /// if let CommandErrorKind::ExitError(1i32, output) = error.source {
    ///     assert_eq!(
    ///         String::from_utf8_lossy(&output.stderr).to_string(),
    ///         "this command failed with this stderr\n",
    ///     );
    /// } else {
    ///     panic!("should fail with stderr output");
    /// }
    ///
    /// let error = Command::new("foobar").result().unwrap_err();
    /// assert!(matches!(error.source, CommandErrorKind::IoError(_)));
    /// assert_eq!(error.command, "foobar");
    /// ```
    fn result(&mut self) -> CommandResult;
}

impl CommandExt for osCommand {
    fn inherit(&mut self) -> &mut Self {
        use std::process::Stdio;
        self.stdout(Stdio::inherit()).stderr(Stdio::inherit())
    }

    fn log(&mut self) -> &mut Self {
        debug!("Command> {:?}", self.display());
        self
    }

    fn print(&mut self) -> &mut Self {
        println!("Command> {:?}", self.display());
        self
    }

    fn display(&self) -> String {
        format!("{:?}", self).replace("\"", "")
    }

    // NOTE: output checks an internal command error
    // and maps it to specific command errors
    //
    // -- ideally we can match by string below --
    //
    // - an issue is that we may have  a needed upgrade error occur under "None".
    //   CommandErrorKind terminated versus ExitErrors

    fn result(&mut self) -> CommandResult {
        info!("executing command {}", self.display());
        self.output()
            .map_err(|e| CommandError {
                command: self.display(),
                source: CommandErrorKind::IoError(e),
            })
            .and_then(|output| match output.status.code() {
                Some(0i32) => Ok(output),
                None => {
                    error!(
                        "command error occured with {}. a command error kind occured",
                        self.display()
                    );
                    return Err(CommandError {
                        command: self.display(),
                        source: CommandErrorKind::Terminated,
                    });
                }
                Some(code) => {
                    error!(
                        "an error occured with command {:?}, code {:?} and output {:?}",
                        self.display(),
                        code,
                        output
                    );
                    if let Err(helm_error) = check_connectivity_error(output.stderr.clone()) {
                        return Err(CommandError {
                            command: self.display(),
                            source: CommandErrorKind::ConnectivityError(helm_error),
                        });
                    }
                    return Err(CommandError {
                        command: self.display(),
                        source: CommandErrorKind::ExitError(code, output),
                    });
                }
            })
    }
}

#[derive(Debug)]
pub enum CommandConnectivityError {
    Error(String),
}

fn check_connectivity_error(stderr: Vec<u8>) -> Result<(), CommandConnectivityError> {
    if !stderr.is_empty() {
        let stderr_str = String::from_utf8(stderr)
            .map_err(|e| CommandConnectivityError::Error(e.to_string()))?;
        if stderr_str.contains("Kubernetes cluster unreachable") {
            error!("kubernetes cluster unreachable");
            return Err(CommandConnectivityError::Error(stderr_str));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_output_display() {
        let error = osCommand::new("ls")
            .arg("does-not-exist")
            .result()
            .unwrap_err();
        let error_display = format!("{}", error.source);
        assert!(error_display.starts_with("Child process completed with non-zero exit code"));
        assert!(error_display.contains("stdout:"));
        assert!(error_display.contains("stderr:"));
    }

    #[test]
    fn test_output_print() {
        let error = osCommand::new("ls").arg("does-not-exist").print();
    }

    #[test]
    fn test_output_new() {
        let error = osCommand::new("ls").arg("does-not-exist").new();
    }

    #[test]
    fn test_output_result() {
        let error = osCommand::new("ls").arg("does-not-exist").result();
    }
}
