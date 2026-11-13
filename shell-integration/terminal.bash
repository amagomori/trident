[[ $- == *i* ]] || return
[[ ${_terminal_integration_loaded-} ]] && return
_terminal_integration_loaded=1
PS1='\[\e]133;A\a\]'"${PS1-}"'\[\e]133;B\a\]'
if (( BASH_VERSINFO[0] < 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] < 4) )); then
    return
fi

_terminal_prompt_command() {
    local exit_code=$?
    printf '\033]133;D;%s\007' "$exit_code"
    return "$exit_code"
}

if (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1) )) \
    && [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == 'declare -a '* ]]; then
    PROMPT_COMMAND=(_terminal_prompt_command "${PROMPT_COMMAND[@]}")
else
    PROMPT_COMMAND="_terminal_prompt_command${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
fi
PS0='\e]133;C\a'"${PS0-}"
