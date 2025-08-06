// Disables stdin from being echoed back to the terminal.
const SILENCE_INPUT: &str = "stty -echo";

// Disables bracketed paste mode which adds a lot of noise to the output.
const DISABLE_BRACKETED_PASTE: &str = "bind 'set enable-bracketed-paste off'";

// Sets the prompt to include the exit code of the last standalone command.
const PROMPT: &str = "PS1='{MARKER}$?:'";

// Disables the input prompt, should never be used anyway but just in case.
const PROMPT_2: &str = "PS2=''";

// Since we use the prompts to detect when commands finish, we need to make sure they are not overwritten
// to avoid jailbreaks in the simulation.
const READONLY_PROMPTS: &str = "readonly PS1; readonly PS2";

// Overrides the default exit command to not exit the shell.
// TODO: echo special marker on exit to terminate the session.
const EXIT_COMMAND: &str = "exit() { return 0; }; export -f exit";

/// Builds the command to configure the shell.
pub fn conf_cmd(marker: &str) -> String {
    let prompt = &PROMPT.replace("{MARKER}", marker);
    let init_cmds = vec![
        SILENCE_INPUT,
        DISABLE_BRACKETED_PASTE,
        prompt,
        PROMPT_2,
        READONLY_PROMPTS,
        EXIT_COMMAND,
    ];

    format!("{}\n", init_cmds.join("; "))
}

// TODO: support any POSIX shell
pub fn standalone_cmd(cmd: &str) -> Vec<String> {
    vec!["/bin/bash".to_string(), "-c".to_string(), cmd.to_string()]
}
pub fn init_cmd() -> Vec<String> {
    vec!["/bin/bash".to_string(), "-i".to_string()]
}
