use const_format::{concatcp, formatcp};

// Change this
pub const UNIQUE_MARKER: &str = "TR0N-F1GHTS-4-TH3-U23R2";

pub const PS1_MARKER: &str = formatcp!("#PS1-{}#:", UNIQUE_MARKER);
pub const PS2_MARKER: &str = formatcp!("#PS2-{}#:", UNIQUE_MARKER);
pub const EXIT_MARKER: &str = formatcp!("#EXIT-{}#:", UNIQUE_MARKER);

const PS1: &str = formatcp!("{}$?:", PS1_MARKER); // Also includes the exit code
const PS2: &str = formatcp!("{}", PS2_MARKER);

// Disables stdin from being echoe back to the terminal.
const SILENCE_INPUT: &str = "stty -echo; ";

// Capture exit code of pipes, good for reward shaping.
const FAIL_ON_PIPE_FAILURE: &str = "set -o pipefail; ";

// Disables bracketed paste mode which adds a lot of noise to the output.
const DISABLE_BRACKETED_PASTE: &str = "bind 'set enable-bracketed-paste off'; ";

// Sets the prompt to include the exit code of the last standalone command.
const SET_PS1: &str = formatcp!("PS1='{}'; ", PS1);

// Disables the input prompt, should never be used anyway but just in case.
const SET_PS2: &str = formatcp!("PS2='{}'; ", PS2);

// Since we use the prompts to detect when commands finish, we need to make sure they are not overwritten
// to avoid jailbreaks in the simulation. Agent can still echo the prompts though.
const READONLY_PROMPTS: &str = "readonly PS1; readonly PS2; ";

// Overrides the default exit command to not exit the shell.
// TODO: echo special marker on exit to terminate the session.
const EXIT_COMMAND: &str = formatcp!("exit() {{ echo '{}'; return 0; }}; export -f exit; ", EXIT_MARKER);

// Ignore EOF to prevent the shell from exiting when the input stream is closed.
const IGNORE_EOF: &str = "set -o ignoreeof; ";

/// Builds the command to configure the shell.
pub const CONF_CMD: &str = concatcp!(
    SILENCE_INPUT,
    DISABLE_BRACKETED_PASTE,
    SET_PS1,
    SET_PS2,
    READONLY_PROMPTS,
    EXIT_COMMAND,
    FAIL_ON_PIPE_FAILURE,
    IGNORE_EOF,
    "\n"
);

// TODO: support any POSIX shell
pub fn standalone_cmd(cmd: &str) -> Vec<String> {
    vec!["/bin/bash".to_string(), "-c".to_string(), cmd.to_string()]
}
pub fn init_cmd() -> Vec<String> {
    vec!["/bin/bash".to_string(), "-i".to_string()]
}
