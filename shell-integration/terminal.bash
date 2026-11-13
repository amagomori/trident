[[ $- == *i* ]] || return
[[ ${_terminal_integration_loaded-} ]] && return
_terminal_integration_loaded=1
_terminal_mark_prompt() {
    [[ ${PS1-} == *'\[\e]133;A\a\]'* ]] || PS1='\[\e]133;A\a\]'"${PS1-}"
    [[ ${PS1-} == *'\[\e]133;B\a\]'* ]] || PS1="${PS1-}"'\[\e]133;B\a\]'
}
_terminal_mark_prompt

_terminal_save_status() {
    _terminal_command_status=$?
    return "$_terminal_command_status"
}

_terminal_prompt_command() {
    local exit_code=$_terminal_command_status
    _terminal_mark_prompt
    if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
        printf '\033]133;D;%s\007' "$exit_code"
    fi
    return "$exit_code"
}

if (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1) )) \
    && [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == 'declare -a '* ]]; then
    PROMPT_COMMAND=(_terminal_save_status "${PROMPT_COMMAND[@]}" _terminal_prompt_command)
else
    PROMPT_COMMAND="_terminal_save_status${PROMPT_COMMAND:+$'\n'$PROMPT_COMMAND}"$'\n_terminal_prompt_command'
fi
if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
    PS0='\e]133;C\a'"${PS0-}"
fi
