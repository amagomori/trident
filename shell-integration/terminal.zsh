# Report prompt/command boundaries (OSC 133) and the working directory (OSC 7).
# PROMPT_EOL_MARK reports status before zsh draws its partial-line marker; precmd
# reports it again for ordinary prompts. Both preserve the command's exit status.
[[ -o interactive ]] || return
(( ${+_terminal_integration_loaded} )) && return
typeset -g _terminal_integration_loaded=1
PROMPT_EOL_MARK=$'%{\e]133;D;%?\a%}'"${PROMPT_EOL_MARK-%B%S%#%s%b}"

autoload -Uz add-zsh-hook
_terminal_report_cwd() {
    emulate -L zsh
    setopt extendedglob
    local LC_ALL=C
    printf '\033]7;file://%s%s\007' "$HOST" "${PWD//(#m)[^a-zA-Z0-9\/._~-]/%${(l:2::0:)$(( [##16] #MATCH ))}}"
}
_terminal_precmd() {
    local exit_code=$?
    printf '\033]133;D;%s\007' "$exit_code"
    _terminal_report_cwd
    return "$exit_code"
}
_terminal_preexec() {
    printf '\033]133;C\007'
}
add-zsh-hook precmd _terminal_precmd
add-zsh-hook preexec _terminal_preexec
PS1=$'%{\e]133;A\a%}'"${PS1}"$'%{\e]133;B\a%}'
