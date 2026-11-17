[[ -o interactive ]] || return
(( ${+_terminal_integration_loaded} )) && return
typeset -g _terminal_integration_loaded=1
# On-demand only: never send an entire buffer on each keystroke. The terminal
# validates it against the rendered cells before sending any cursor keys.
_terminal_prompt_input() {
    local encoded unit=scalar
    [[ -o combiningchars ]] && unit=glyph
    encoded=$(printf '%s\n' "$BUFFER" | base64 | tr -d '\r\n')
    printf '\e]777;trident;prompt-input;data;%s;%s;%s\a' "$CURSOR" "$encoded" "$unit"
}
zle -N _terminal_prompt_input
for _terminal_keymap in emacs viins vicmd; do
    bindkey -M "$_terminal_keymap" $'\e[1114109u' _terminal_prompt_input
done
unset _terminal_keymap
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
PS1=$'%{\e]133;A\a%}'"${PS1}"$'%{\e]777;trident;prompt-input;ready\a\e]133;B\a%}'
